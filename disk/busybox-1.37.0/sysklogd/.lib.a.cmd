cmd_sysklogd/lib.a := rm -f sysklogd/lib.a; riscv64-linux-musl-ar  rcs sysklogd/lib.a sysklogd/klogd.o sysklogd/logread.o sysklogd/syslogd_and_logger.o
