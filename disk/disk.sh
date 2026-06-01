#!/bin/sh

BUSY_BOX_DIR=busybox-1.37.0/_install

set -e
mkdir -p tmp

target=/dev/sda

# 创建 MBR 分区表
sudo parted -s ${target} mklabel msdos
sudo parted -s ${target} mkpart primary fat32 1MiB 257MiB
sudo parted -s ${target} mkpart primary ext4 257MiB 1281MiB
sudo parted -s ${target} set 1 boot on

# 格式化分区
sudo mkfs.vfat -F32 ${target}1 -n boot
sudo mkfs.ext4 ${target}2 -L rootfs

# 挂载 boot 分区并复制文件
sudo mount ${target}1 tmp
sudo cp boot/* tmp/
sudo umount tmp

# 挂载 rootfs 分区并复制文件
sudo mount ${target}2 tmp
sudo cp -a ${BUSY_BOX_DIR}/* tmp/
sudo umount tmp

rm -rf tmp
