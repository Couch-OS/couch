/*
 * hci_stp: BlueZ HCI driver over the MediaTek STP transport (Couch, HA100).
 *
 * The vendor tree offers Bluetooth to userspace only as /dev/stpbt, a raw
 * character device that Android's Bluedroid drives directly. BlueZ needs an
 * hci_dev, which Couch has so far provided by pumping /dev/stpbt through the
 * virtual HCI driver from userspace. That pump is where every timing hazard
 * of the combo radio surfaces (STP flow control, whole-chip resets, the
 * one-second vhci auto-create). This driver registers an hci_dev directly
 * on the STP export API instead, using the "BlueZ mode" the STP core already
 * carries (mtk_wcn_stp_set_bluez + mtk_wcn_stp_register_if_rx), so received
 * packets are handed to the Bluetooth core from the STP receive path and
 * transmits go through mtk_wcn_stp_send_data with the STP layer's own flow
 * control.
 *
 * Lifecycle mirrors stp_chrdev_bt: the radio is powered (WMT function on)
 * in hdev->open and off in hdev->close, so the BT function is on exactly
 * while the adapter is up. Built as a module, it is loaded when the user
 * turns Bluetooth on and unloaded when they turn it off.
 */
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/skbuff.h>
#include <linux/workqueue.h>
#include <linux/delay.h>
#include <net/bluetooth/bluetooth.h>
#include <net/bluetooth/hci_core.h>
#include "osal_typedef.h"
#include "stp_exp.h"
#include "wmt_exp.h"

#define HCI_STP_NAME "hci_stp"
/* mtk_wcn_stp_send_data returns 0 while the STP window is closed. */
#define HCI_STP_TX_TRIES 200

static struct hci_dev *hci_stp_hdev;
static struct sk_buff_head hci_stp_txq;
static struct work_struct hci_stp_tx_work;
static atomic_t hci_stp_reset_pending = ATOMIC_INIT(0);

/* H4 reassembly. The STP payload is the H4 stream the radio produces:
 * packet type byte, header, payload, and an STP packet may hold a partial
 * or several HCI packets. hci_recv_stream_fragment did this in 3.18 and is
 * gone from 4.1 on, so the driver carries its own and works on both cores.
 * Only the STP receive thread calls this, so no lock. */
static struct sk_buff *hci_stp_rx_skb;
static unsigned int hci_stp_rx_need;
static bool hci_stp_rx_in_header;

static void hci_stp_rx_reset(void)
{
	kfree_skb(hci_stp_rx_skb);
	hci_stp_rx_skb = NULL;
	hci_stp_rx_need = 0;
}

static void hci_stp_rx(const UINT8 *data, INT32 size)
{
	struct hci_dev *hdev = hci_stp_hdev;

	if (!hdev || !test_bit(HCI_RUNNING, &hdev->flags) || size <= 0)
		return;
	while (size > 0) {
		unsigned int take;

		if (!hci_stp_rx_skb) {
			u8 type = *data++;

			size--;
			switch (type) {
			case HCI_EVENT_PKT:
				hci_stp_rx_need = HCI_EVENT_HDR_SIZE;
				break;
			case HCI_ACLDATA_PKT:
				hci_stp_rx_need = HCI_ACL_HDR_SIZE;
				break;
			case HCI_SCODATA_PKT:
				hci_stp_rx_need = HCI_SCO_HDR_SIZE;
				break;
			default:
				BT_ERR("%s: unknown H4 packet type 0x%02x, dropping %d bytes",
				       HCI_STP_NAME, type, size);
				hdev->stat.err_rx++;
				return;
			}
			hci_stp_rx_skb = bt_skb_alloc(HCI_MAX_FRAME_SIZE, GFP_ATOMIC);
			if (!hci_stp_rx_skb) {
				hci_stp_rx_need = 0;
				return;
			}
			bt_cb(hci_stp_rx_skb)->pkt_type = type;
			hci_stp_rx_in_header = true;
			continue;
		}
		take = min_t(unsigned int, hci_stp_rx_need, (unsigned int)size);
		if (skb_tailroom(hci_stp_rx_skb) < take) {
			BT_ERR("%s: oversized packet, dropping", HCI_STP_NAME);
			hci_stp_rx_reset();
			hdev->stat.err_rx++;
			return;
		}
		memcpy(skb_put(hci_stp_rx_skb, take), data, take);
		data += take;
		size -= take;
		hci_stp_rx_need -= take;
		if (hci_stp_rx_need)
			continue;
		if (hci_stp_rx_in_header) {
			const u8 *h = hci_stp_rx_skb->data;

			hci_stp_rx_in_header = false;
			switch (bt_cb(hci_stp_rx_skb)->pkt_type) {
			case HCI_EVENT_PKT:
				hci_stp_rx_need = h[1];
				break;
			case HCI_ACLDATA_PKT:
				hci_stp_rx_need = h[2] | (h[3] << 8);
				break;
			default:
				hci_stp_rx_need = h[2];
				break;
			}
			if (hci_stp_rx_need)
				continue;
		}
		hdev->stat.byte_rx += hci_stp_rx_skb->len + 1;
		hci_recv_frame(hdev, hci_stp_rx_skb);
		hci_stp_rx_skb = NULL;
	}
}

/* The STP core treats a function without an event callback as inactive, so
 * one must be registered even though packets arrive through hci_stp_rx. */
static void hci_stp_event(void)
{
}

static void hci_stp_tx_resume(void)
{
	schedule_work(&hci_stp_tx_work);
}

static void hci_stp_tx_worker(struct work_struct *work)
{
	struct hci_dev *hdev = hci_stp_hdev;
	struct sk_buff *skb;

	while ((skb = skb_dequeue(&hci_stp_txq)) != NULL) {
		int tries = 0;
		INT32 written = 0;

		if (!hdev || !test_bit(HCI_RUNNING, &hdev->flags)) {
			kfree_skb(skb);
			continue;
		}
		for (;;) {
			written = mtk_wcn_stp_send_data(skb->data, skb->len, BT_TASK_INDX);
			if (written == skb->len)
				break;
			if (written < 0 || ++tries > HCI_STP_TX_TRIES) {
				BT_ERR("%s: send failed (%d) after %d tries, dropping %u bytes",
				       HCI_STP_NAME, written, tries, skb->len);
				hdev->stat.err_tx++;
				break;
			}
			/* No window space: the STP tx event callback also reschedules
			 * us, but a short sleep keeps this frame ahead of later ones. */
			msleep(5);
		}
		if (written == skb->len) {
			switch (bt_cb(skb)->pkt_type) {
			case HCI_COMMAND_PKT:
				hdev->stat.cmd_tx++;
				break;
			case HCI_ACLDATA_PKT:
				hdev->stat.acl_tx++;
				break;
			case HCI_SCODATA_PKT:
				hdev->stat.sco_tx++;
				break;
			}
			hdev->stat.byte_tx += skb->len;
		}
		kfree_skb(skb);
	}
}

static int hci_stp_send(struct hci_dev *hdev, struct sk_buff *skb)
{
	/* Prepend the H4 packet type; STP carries the H4 stream as-is. */
	memcpy(skb_push(skb, 1), &bt_cb(skb)->pkt_type, 1);
	skb_queue_tail(&hci_stp_txq, skb);
	schedule_work(&hci_stp_tx_work);
	return 0;
}

static void hci_stp_rst_cb(ENUM_WMTDRV_TYPE_T src, ENUM_WMTDRV_TYPE_T dst,
			   ENUM_WMTMSG_TYPE_T type, void *buf, unsigned int sz)
{
	ENUM_WMTRSTMSG_TYPE_T rst = WMTRSTMSG_RESET_INVALID;

	if (sz > sizeof(rst) || src != WMTDRV_TYPE_WMT || dst != WMTDRV_TYPE_BT ||
	    type != WMTMSG_TYPE_RESET)
		return;
	memcpy(&rst, buf, sz);
	if (rst == WMTRSTMSG_RESET_START) {
		atomic_set(&hci_stp_reset_pending, 1);
		BT_ERR("%s: whole-chip reset started", HCI_STP_NAME);
	} else if (rst == WMTRSTMSG_RESET_END || rst == WMTRSTMSG_RESET_END_FAIL) {
		atomic_set(&hci_stp_reset_pending, 0);
		BT_ERR("%s: whole-chip reset %s; the adapter needs a power cycle",
		       HCI_STP_NAME, rst == WMTRSTMSG_RESET_END ? "ended" : "failed");
	}
}

static int hci_stp_open(struct hci_dev *hdev)
{
	if (test_and_set_bit(HCI_RUNNING, &hdev->flags))
		return 0;
	if (mtk_wcn_wmt_func_on(WMTDRV_TYPE_BT) == MTK_WCN_BOOL_FALSE) {
		BT_ERR("%s: WMT refused to power Bluetooth on", HCI_STP_NAME);
		clear_bit(HCI_RUNNING, &hdev->flags);
		return -ENODEV;
	}
	if (!mtk_wcn_stp_is_ready()) {
		BT_ERR("%s: STP is not ready", HCI_STP_NAME);
		mtk_wcn_wmt_func_off(WMTDRV_TYPE_BT);
		clear_bit(HCI_RUNNING, &hdev->flags);
		return -ENODEV;
	}
	atomic_set(&hci_stp_reset_pending, 0);
	hci_stp_rx_reset();
	mtk_wcn_stp_register_if_rx(hci_stp_rx);
	mtk_wcn_stp_register_event_cb(BT_TASK_INDX, hci_stp_event);
	mtk_wcn_stp_register_tx_event_cb(BT_TASK_INDX, hci_stp_tx_resume);
	mtk_wcn_stp_set_bluez(MTK_WCN_BOOL_TRUE);
	mtk_wcn_wmt_msgcb_reg(WMTDRV_TYPE_BT, hci_stp_rst_cb);
	BT_INFO("%s: Bluetooth on through STP (BlueZ mode)", HCI_STP_NAME);
	return 0;
}

static int hci_stp_close(struct hci_dev *hdev)
{
	if (!test_and_clear_bit(HCI_RUNNING, &hdev->flags))
		return 0;
	cancel_work_sync(&hci_stp_tx_work);
	skb_queue_purge(&hci_stp_txq);
	mtk_wcn_wmt_msgcb_unreg(WMTDRV_TYPE_BT);
	mtk_wcn_stp_set_bluez(MTK_WCN_BOOL_FALSE);
	mtk_wcn_stp_register_if_rx(NULL);
	mtk_wcn_stp_register_event_cb(BT_TASK_INDX, NULL);
	mtk_wcn_stp_register_tx_event_cb(BT_TASK_INDX, NULL);
	hci_stp_rx_reset();
	if (mtk_wcn_wmt_func_off(WMTDRV_TYPE_BT) == MTK_WCN_BOOL_FALSE)
		BT_ERR("%s: WMT failed to power Bluetooth off", HCI_STP_NAME);
	else
		BT_INFO("%s: Bluetooth off", HCI_STP_NAME);
	return 0;
}

static int hci_stp_flush(struct hci_dev *hdev)
{
	skb_queue_purge(&hci_stp_txq);
	return 0;
}

static int __init hci_stp_init(void)
{
	struct hci_dev *hdev;
	int err;

	skb_queue_head_init(&hci_stp_txq);
	INIT_WORK(&hci_stp_tx_work, hci_stp_tx_worker);
	hdev = hci_alloc_dev();
	if (!hdev)
		return -ENOMEM;
	hdev->bus = HCI_VIRTUAL;
	hdev->open = hci_stp_open;
	hdev->close = hci_stp_close;
	hdev->flush = hci_stp_flush;
	hdev->send = hci_stp_send;
	hci_stp_hdev = hdev;
	err = hci_register_dev(hdev);
	if (err < 0) {
		hci_stp_hdev = NULL;
		hci_free_dev(hdev);
		return err;
	}
	BT_INFO("%s: registered %s on the MediaTek STP transport", HCI_STP_NAME, hdev->name);
	return 0;
}

static void __exit hci_stp_exit(void)
{
	struct hci_dev *hdev = hci_stp_hdev;

	if (!hdev)
		return;
	hci_unregister_dev(hdev);
	hci_stp_hdev = NULL;
	hci_free_dev(hdev);
}

module_init(hci_stp_init);
module_exit(hci_stp_exit);

MODULE_AUTHOR("Couch");
MODULE_DESCRIPTION("BlueZ HCI driver over the MediaTek STP transport");
MODULE_LICENSE("GPL");
