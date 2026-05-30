#!/bin/sh
set -e

# sh run.sh readobj
# sh run.sh build <plat>
# sudo mount -o loop -t ext4 rootfs-riscv64.img tmp/

CMD=$1
PLAT=$2

if [ "$CMD" = "readobj" ]; then
    rust-readobj -a target/riscv64gc-unknown-none-elf/release/starryos > starryos.obj
elif [ "$CMD" = "build" ]; then
    if [ -z "$PLAT" ]; then
        echo "Usage: sh run.sh build <plat>"
        exit 1
    fi

    cargo starry build -c $PLAT.toml
    cp target/riscv64gc-unknown-none-elf/release/starryos starry-$PLAT.elf
    cp target/riscv64gc-unknown-none-elf/release/starryos.bin starry-$PLAT.bin

    # 构造 FIT 镜像
    cp target/riscv64gc-unknown-none-elf/release/starryos.bin uboot/kernel
    cd uboot
    mkimage -f boot.its boot.sd

    # 检查是否存在 /dev/sda1
    # 如果存在，则说明已经插入了 SD 卡，并且可以将生成的 boot.sd 复制到 SD 卡的 boot 分区中
    # 如果存在，则将 starry-sg2002.bin 复制到该目录下
    if [ -e "/dev/sda1" ]; then
        echo "SD card detected. Copying boot.sd to SD card..."
        sudo mkdir -p /run/media/linkwanna/boot
        sudo mount /dev/sda1 /run/media/linkwanna/boot
        sudo cp boot.sd /run/media/linkwanna/boot
        sudo sync
        sudo umount /run/media/linkwanna/boot
        sudo rm -r /run/media/linkwanna/boot
    fi
else
    echo "Usage: sh run.sh <command>"
    echo "Commands:"
    echo "  readobj       Read object files and generate C code"
    echo "  build <plat>  Build the project for the specified platform"
    exit 1
fi
