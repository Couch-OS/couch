/*
 * keytap: count keypad presses and releases from an evdev node.
 *
 * Acceptance test for the HA100 keypad driver change (8 ms debounce, hold
 * polling): tap a key N times quickly and the press count must be N. Reads
 * the mt_gpio_kpd event node (found by name unless a path is given), prints
 * every EV_KEY event with the kernel timestamp and, on SIGINT or after
 * --seconds, a per-key summary of presses and releases.
 *
 *   keytap [--seconds N] [--quiet] [/dev/input/eventN]
 *
 * Static ARM build: tools/keypad/build.sh on Ollie (zig cc). On the remote:
 *   /tmp/keytap --seconds 10        # then tap DOWN 20 times
 *
 * Copyright (c) 2026 Couch contributors. GPL-2.0-or-later.
 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

#define DEVICE_NAME "mt_gpio_kpd"

/* musl 1.2 on 32-bit ARM has a 64-bit time_t; linux/input.h then exposes the
 * kernel's 32-bit event timestamp through these accessors instead of .time. */
#ifndef input_event_sec
#define input_event_sec time.tv_sec
#define input_event_usec time.tv_usec
#endif

static volatile sig_atomic_t stop;
static void on_signal(int sig) { (void)sig; stop = 1; }

static unsigned long presses[KEY_MAX + 1], releases[KEY_MAX + 1];
static unsigned long total_press, total_release, repeats;

static int open_named(char *path, size_t len)
{
	DIR *dir = opendir("/dev/input");
	struct dirent *ent;
	char name[64];

	if (!dir)
		return -1;
	while ((ent = readdir(dir))) {
		int fd;

		if (strncmp(ent->d_name, "event", 5))
			continue;
		snprintf(path, len, "/dev/input/%s", ent->d_name);
		fd = open(path, O_RDONLY | O_NONBLOCK);
		if (fd < 0)
			continue;
		if (ioctl(fd, EVIOCGNAME(sizeof(name)), name) > 0 &&
		    strstr(name, DEVICE_NAME)) {
			closedir(dir);
			return fd;
		}
		close(fd);
	}
	closedir(dir);
	errno = ENOENT;
	return -1;
}

static void summary(double elapsed)
{
	int code;

	printf("\n--- %.1f s: %lu presses, %lu releases", elapsed, total_press,
	       total_release);
	if (repeats)
		printf(", %lu autorepeat events (should be 0)", repeats);
	printf("\n");
	for (code = 0; code <= KEY_MAX; code++)
		if (presses[code] || releases[code])
			printf("key %3d: %4lu presses %4lu releases\n", code,
			       presses[code], releases[code]);
	fflush(stdout);
}

int main(int argc, char **argv)
{
	char path[64] = "";
	const char *given = NULL;
	double seconds = 0;
	int quiet = 0, fd, i;
	struct timespec start, now;
	long first_sec = 0, first_usec = 0;
	int have_first = 0;

	for (i = 1; i < argc; i++) {
		if (!strcmp(argv[i], "--seconds") && i + 1 < argc)
			seconds = atof(argv[++i]);
		else if (!strcmp(argv[i], "--quiet"))
			quiet = 1;
		else if (argv[i][0] == '-') {
			fprintf(stderr, "usage: keytap [--seconds N] [--quiet] [/dev/input/eventN]\n");
			return 2;
		} else
			given = argv[i];
	}

	if (given) {
		snprintf(path, sizeof(path), "%s", given);
		fd = open(path, O_RDONLY | O_NONBLOCK);
	} else {
		fd = open_named(path, sizeof(path));
	}
	if (fd < 0) {
		fprintf(stderr, "keytap: cannot open %s: %s\n",
			given ? given : DEVICE_NAME, strerror(errno));
		return 1;
	}
	{
		char name[64] = "?";
		ioctl(fd, EVIOCGNAME(sizeof(name)), name);
		printf("reading %s (%s)%s\n", path, name,
		       seconds > 0 ? "" : ", Ctrl-C for the summary");
		fflush(stdout);
	}

	signal(SIGINT, on_signal);
	signal(SIGTERM, on_signal);
	clock_gettime(CLOCK_MONOTONIC, &start);

	while (!stop) {
		struct input_event ev[32];
		struct timeval tv = {0, 100000};
		fd_set set;
		ssize_t n;
		int k;

		if (seconds > 0) {
			clock_gettime(CLOCK_MONOTONIC, &now);
			if ((now.tv_sec - start.tv_sec) +
			    (now.tv_nsec - start.tv_nsec) / 1e9 >= seconds)
				break;
		}
		FD_ZERO(&set);
		FD_SET(fd, &set);
		if (select(fd + 1, &set, NULL, NULL, &tv) <= 0)
			continue;
		n = read(fd, ev, sizeof(ev));
		if (n < 0) {
			if (errno == EAGAIN || errno == EINTR)
				continue;
			perror("keytap: read");
			break;
		}
		for (k = 0; k < (int)(n / sizeof(ev[0])); k++) {
			double t;

			if (ev[k].type != EV_KEY || ev[k].code > KEY_MAX)
				continue;
			if (!have_first) {
				first_sec = ev[k].input_event_sec;
				first_usec = ev[k].input_event_usec;
				have_first = 1;
			}
			/* The accessors are unsigned in the kernel header; subtract
			 * as signed or a usec underflow prints ~4295 s. */
			t = ((long)ev[k].input_event_sec - first_sec) +
			    ((long)ev[k].input_event_usec - first_usec) / 1e6;
			if (ev[k].value == 1) {
				presses[ev[k].code]++;
				total_press++;
			} else if (ev[k].value == 0) {
				releases[ev[k].code]++;
				total_release++;
			} else {
				repeats++;
			}
			if (!quiet) {
				printf("%10.4f  key %3d  %s\n", t, ev[k].code,
				       ev[k].value == 1 ? "press" :
				       ev[k].value == 0 ? "release" : "repeat");
				fflush(stdout);
			}
		}
	}

	clock_gettime(CLOCK_MONOTONIC, &now);
	summary((now.tv_sec - start.tv_sec) + (now.tv_nsec - start.tv_nsec) / 1e9);
	close(fd);
	return 0;
}
