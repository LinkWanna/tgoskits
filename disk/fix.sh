#!/bin/sh

BUSY_BOX_DIR=busybox-1.37.0/_install

set -e
mkdir -p tmp

target=/dev/sda

# 因为我们经常直接断电，有时候 ext4 分区会坏掉，这里通过格式化来修复它
sudo mkfs.ext4 ${target}2 -L rootfs

# 挂载 rootfs 分区并复制文件
sudo mount ${target}2 tmp
sudo cp -a ${BUSY_BOX_DIR}/* tmp/
sudo umount tmp

rm -rf tmp
