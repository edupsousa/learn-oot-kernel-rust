// SPDX-License-Identifier: GPL-2.0
// Userspace test for /dev/hello (lesson 3). Built static so it can run in the busybox VM.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* Must match hello_dev.rs: same magic byte and command numbers. */
#define HELLO_IOC_MAGIC 'h'
#define HELLO_SAY       _IO(HELLO_IOC_MAGIC, 1)
#define HELLO_GET_VALUE _IOR(HELLO_IOC_MAGIC, 2, int)
#define HELLO_SET_VALUE _IOW(HELLO_IOC_MAGIC, 3, int)

/* A number that no ioctl of ours uses, to check the driver answers ENOTTY. */
#define HELLO_UNKNOWN_NR 0x7f

#define TEST_VALUE 42			/* stored with SET_VALUE, expected back from GET_VALUE */
#define BAD_USER_ADDR ((void *)8)	/* never mapped in userspace: the kernel must return EFAULT */
#define MSG "hi kernel"			/* written to the device, expected back from read() */
#define READ_BUF_LEN 64

int main(void)
{
	int fd = open("/dev/hello", O_RDWR);
	int v = TEST_VALUE, out = 0;
	char buf[READ_BUF_LEN] = {0};

	if (fd < 0) { perror("open"); return 1; }
	if (ioctl(fd, HELLO_SAY) < 0) perror("say");
	if (ioctl(fd, HELLO_SET_VALUE, &v) < 0) perror("set");
	if (ioctl(fd, HELLO_GET_VALUE, &out) < 0) perror("get");
	printf("value roundtrip: %d (expect %d)\n", out, TEST_VALUE);

	if (ioctl(fd, HELLO_GET_VALUE, BAD_USER_ADDR) < 0)
		printf("bad user pointer -> %s (expect Bad address)\n", strerror(errno));
	if (ioctl(fd, _IO(HELLO_IOC_MAGIC, HELLO_UNKNOWN_NR)) < 0)
		printf("unknown ioctl -> %s (expect Not a tty, i.e. ENOTTY)\n", strerror(errno));

	write(fd, MSG, strlen(MSG));
	ssize_t n = read(fd, buf, sizeof(buf) - 1);
	printf("read back %zd bytes: %s\n", n, buf);
	close(fd);
	return 0;
}
