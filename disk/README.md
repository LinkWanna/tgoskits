
## BusyBox

1. 下载 busybox-1.37.0.tar.bz2 (https://busybox.net/downloads/busybox-1.37.0.tar.bz2)
2. 安装工具链 `riscv64-linux-musl-`
3. `make menuconfig` 中去掉 `SH1` 和 `SHA256`，并且选择 `static` 进行静态编译

```sh
make ARCH=riscv CROSS_COMPILE=riscv64-linux-musl- CONFIG_PREFIX=$(pwd)/_install install
```

## SD 卡分区与文件系统构造

1. 插入 SD 卡，使用 `lsblk` 确认 SD 卡设备名称（例如 `/dev/sdX`），并修改 `target` 的值为正确的设备名称
2. 执行 `sh disk.sh` 脚本，该脚本会自动进行以下操作：
   - 创建两个分区：一个 256MB 的 FAT32 分区和一个 2GB 的 ext4 分区
   - 将官方镜像中的 `boot` 目录中的文件复制到 SD 卡的 FAT32 分区中
   - 将 busybox 构建的根文件系统复制到 SD 卡的 ext4 分区中
3. 可能的修复问题：因为我们经常直接断电，有时候 ext4 分区会坏掉，执行 `sh fix.sh` 脚本进行修复
