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

    # 检查是否存在 /run/media/linkwanna/rootfs 目录
    # 如果存在，则将 starry-sg2002.bin 和 ext4_100m.img 复制到该目录下
    if [ -d "/run/media/linkwanna/boot" ]; then
        sudo cp boot.sd /run/media/linkwanna/boot
        sudo sync
    fi
else
    echo "Usage: sh run.sh <command>"
    echo "Commands:"
    echo "  readobj       Read object files and generate C code"
    echo "  build <plat>  Build the project for the specified platform"
    exit 1
fi
