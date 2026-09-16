#!/bin/sh
# One read-only, allowlisted snapshot. Optional root is for offline fixtures.
# No sleeps, GPIO writes, device ioctls, process signals or transmission.
set -eu
snapshot_root=${1:-}
read_value() {
    # A readable sysfs node can still return ENODATA. Keep the snapshot useful
    # when the gauge intentionally withholds an unmeasured value.
    if [ -r "$snapshot_root$2" ] && value=$(tr -d '\n\r' < "$snapshot_root$2" 2>/dev/null); then
        printf '%s=%s\n' "$1" "$value"
    else
        printf '%s=unavailable\n' "$1"
    fi
}
read_value kernel /proc/sys/kernel/osrelease
read_value uptime /proc/uptime
read_value cpu.online /sys/devices/system/cpu/online
read_value cpu.active_floor /proc/hps/num_base_perf_serv
read_value cpu.governor /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor
read_value cpu.frequency_khz /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq
read_value power.autosleep /sys/power/autosleep
for supply in battery usb ac wireless; do
    for field in status capacity health online batt_temp batt_vol current_now charge_counter BatteryAverageCurrent BatterySenseVoltage ChargerVoltage; do
        case "$supply:$field" in
            battery:online) continue;;
            battery:*) ;;
            *:online) ;;
            *) continue;;
        esac
        read_value "power.$supply.$field" "/sys/class/power_supply/$supply/$field"
    done
done
# The new kernel supplies one cached sample. Never dump future/unrecognized
# fields into diagnostics, and never turn absent telemetry into zero.
if [ -r "$snapshot_root/sys/class/power_supply/battery/couch_gauge" ] &&
   gauge=$(cat "$snapshot_root/sys/class/power_supply/battery/couch_gauge" 2>/dev/null); then
    printf '%s\n' "$gauge" | awk -F= '
    NF == 2 {
        numeric = " schema ready sequence sampled_boottime_seconds soc_percent ui_soc_percent charger_online charger_type charger_state full recharging status voltage_mv charger_voltage_mv current_estimate_ua charging_sense_ma temperature_fixed algorithm_temperature_c thermistor_voltage_mv thermistor_resistance_ohm profile_qmax_25_mah model_qmax_mah model_ocv_mv model_resistance_mohm model_discharge_tenth_mah sense_resistance_mohm high_voltage_profile full_current_ma recharge_mv sync_tracking_seconds task_period_seconds "
        if (($2 ~ /^-?[0-9]+$/ && index(numeric, " " $1 " ")) ||
            ($1 == "method" && $2 == "software") ||
            ($1 == "calibration" && $2 == "unverified") ||
            ($1 == "current_estimate_positive" && $2 == "discharge"))
            printf "power.gauge.%s=%s\n", $1, $2
    }'
else
    printf 'power.gauge=unavailable\n'
fi
for led in red button-backlight lcd-backlight; do
    read_value "led.$led.brightness" "/sys/class/leds/$led/brightness"
done
read_value ir.output_telemetry /sys/module/couch_irtx/parameters/output_telemetry
if [ -r "$snapshot_root/sys/class/misc/mtgpio/pin" ]; then
    awk '
    /^[ ]*(14|17|58|61):/ {
        split($0, line, ":"); pin=line[1]+0; gsub(/[ \t]/,"",line[2]);
        if (line[2] !~ /^[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]$/) {
            printf "gpio.%d=unrecognized\n",pin; next
        }
        printf "gpio.%d.mode=%s din=%s dout=%s direction=%s pull_enable=%s\n", pin,
            substr(line[2],1,1),substr(line[2],3,1),substr(line[2],4,1),
            substr(line[2],6,1),substr(line[2],5,1)
    }' "$snapshot_root/sys/class/misc/mtgpio/pin"
else
    printf 'gpio=unavailable\n'
fi
# Health age establishes freshness; two independently requested snapshots
# establish advancement. Never dump process command lines or environment.
if [ -r "$snapshot_root/tmp/couch-gui.health" ] && [ -r "$snapshot_root/proc/uptime" ]; then
    read -r gui_pid gui_stamp extra < "$snapshot_root/tmp/couch-gui.health" || true
    case "${gui_pid:-}:${gui_stamp:-}" in
        *[!0-9:]*|:*|*:) printf 'gui.health=invalid\n';;
        *)
            if [ -z "${extra:-}" ] && [ -r "$snapshot_root/proc/$gui_pid/comm" ] &&
               [ "$(cat "$snapshot_root/proc/$gui_pid/comm")" = couch-gui ]; then
                awk -v stamp="$gui_stamp" '{age=int($1)-stamp; printf "gui.health_age_seconds=%d\ngui.health_fresh=%s\n",age,(age>=0 && age<=10)?"yes":"no"}' "$snapshot_root/proc/uptime"
            else
                printf 'gui.health=process_unverified\n'
            fi;;
    esac
else
    printf 'gui.health=unavailable\n'
fi
