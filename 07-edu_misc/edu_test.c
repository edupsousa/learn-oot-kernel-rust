// SPDX-License-Identifier: GPL-2.0
// Userspace test for /dev/edu (lesson 7). Built static so it can run in the busybox VM.
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* Must match edu_misc.rs: same magic byte and command number. */
#define EDU_IOC_MAGIC 'e'
#define EDU_FACTORIAL _IOWR(EDU_IOC_MAGIC, 1, uint32_t)

#define MAX_INPUT 12		/* 12! is the largest factorial that fits in 32 bits */
#define TOO_BIG   13		/* the driver must answer EINVAL */
#define UNKNOWN_NR 0x7f		/* an ioctl we do not implement: ENOTTY */
#define BAD_USER_ADDR ((void *)8)	/* never mapped: the kernel must answer EFAULT */

static int failures;

static void check(const char *what, int ok)
{
	printf("%-34s %s\n", what, ok ? "ok" : "FAIL");
	if (!ok)
		failures++;
}

int main(void)
{
	int fd = open("/dev/edu", O_RDWR);
	uint32_t v;

	if (fd < 0) { perror("open /dev/edu"); return 1; }

	v = 5;
	check("5! = 120", ioctl(fd, EDU_FACTORIAL, &v) == 0 && v == 120);

	v = MAX_INPUT;
	check("12! = 479001600", ioctl(fd, EDU_FACTORIAL, &v) == 0 && v == 479001600);

	/* Many requests in a row: every one must be woken up by its own interrupt. */
	int all = 1;
	for (uint32_t i = 0; i < 1000; i++) {
		v = i % 13;
		uint32_t want = 1;
		for (uint32_t k = 2; k <= v; k++) want *= k;
		if (ioctl(fd, EDU_FACTORIAL, &v) != 0 || v != want) { all = 0; break; }
	}
	check("1000 requests", all);

	v = TOO_BIG;
	check("13 -> EINVAL", ioctl(fd, EDU_FACTORIAL, &v) < 0 && errno == EINVAL);
	check("bad pointer -> EFAULT", ioctl(fd, EDU_FACTORIAL, BAD_USER_ADDR) < 0 && errno == EFAULT);
	check("unknown ioctl -> ENOTTY", ioctl(fd, _IO(EDU_IOC_MAGIC, UNKNOWN_NR), 0) < 0 && errno == ENOTTY);

	close(fd);
	printf(failures ? "%d FAILED\n" : "all passed\n", failures);
	return failures != 0;
}
