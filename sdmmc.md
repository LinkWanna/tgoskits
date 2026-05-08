1. 适配 `axplat-riscv64-sg2002` 平台
  - 完成 `components/axplat_crates/platforms/axplat-riscv64-sg2002` 的适配
  - 适配 `xuantie-c9xx` 的页表项，在 `axcpu` 中添加 `xuantie-c9xx = ["ax-page-table-entry/xuantie-c9xx"]`

2. 添加 `Cv181xSD` 驱动
  - 利用 `sg200x-bsp` 的 `sdmmc` 进行接口适配
  - 将 `cv181xsd` feature 暴露到 `axfeat` 中

3. 构建 SD 卡分区与文件系统，参考 `disk` 目录

4. 利用 `mkimage` 工具构建 FIT 镜像，包含内核、设备树，用于 SD 卡启动，参考 `uboot` 目录和 `run.sh` 脚本
