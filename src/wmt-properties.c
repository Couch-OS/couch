/* Narrow property_get bridge for the pinned Android WMT launcher only.
 * No property service, persistent files, device identity or network credentials.
 * Build as an ARM shared object without a host libc dependency; the Android
 * process supplies these ordinary Bionic ABI symbols at load time.
 */
extern int open(const char *, int, ...);
extern int close(int);
extern int ioctl(int, unsigned long, ...);
extern int strcmp(const char *, const char *);

static int copy_value(char *output, const char *value) {
    int n = 0;
    if (value) {
        while (n < 91 && value[n]) { output[n] = value[n]; ++n; }
    }
    output[n] = 0;
    return n;
}

static int detected_chip(void) {
    /* MT6580 common_detect/wmt_detect.h: _IOR('w', 0, int).
     * Returns the chip ID previously detected by wmt_loader, not a pointer.
     */
    int transport = open("/dev/stpwmt", 0);
    if (transport < 0) return -1;
    close(transport);
    int fd = open("/dev/wmtdetect", 0);
    if (fd < 0) return -1;
    int chip = ioctl(fd, 0x80047700UL, 0);
    close(fd);
    return chip;
}

int property_get(const char *key, char *output, const char *fallback) {
    if (!strcmp(key, "service.wcn.driver.ready") ||
        !strcmp(key, "persist.mtk.wcn.combo.chipid")) {
        int chip = detected_chip();
        if (chip != 0x6580) return copy_value(output, fallback);
        if (!strcmp(key, "service.wcn.driver.ready")) return copy_value(output, "yes");
        char value[9];
        for (int i = 0; i < 8; ++i) value[i] = "0123456789abcdef"[((unsigned)chip >> ((7-i)*4)) & 15];
        value[8] = 0;
        return copy_value(output, value);
    }
    /* Other optional launcher properties retain their no-service defaults. */
    return copy_value(output, fallback);
}
