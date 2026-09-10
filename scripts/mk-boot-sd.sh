#!/usr/bin/env bash
# Build boot.sd for LicheeRV Nano SD card boot.
#
# Usage: ./scripts/mk-boot-sd.sh
# Output: target/riscv64gc-unknown-none-elf/release/boot.sd

set -euo pipefail
cd "$(dirname "$0")/.."

KERNEL="$(readlink -f target/riscv64gc-unknown-none-elf/release/starryos.bin)"
DTB="$(readlink -f os/StarryOS/configs/board/licheerv-nano-sg2002.dtb)"
ITS="uboot/starry.its"
OUT="$(readlink -f target/riscv64gc-unknown-none-elf/release/boot.sd)"

for f in "${KERNEL}" "${DTB}" "${ITS}"; do
    if [ ! -f "${f}" ]; then
        echo "ERROR: ${f} not found."
        exit 1
    fi
done

# Substitute absolute paths into ITS and build FIT
TMP_ITS="$(mktemp)"
sed -e "s|KERNEL|${KERNEL}|g" -e "s|DTB|${DTB}|g" "${ITS}" > "${TMP_ITS}"

mkimage -f "${TMP_ITS}" "${OUT}"
rm -f "${TMP_ITS}"

echo ""
echo "Created: ${OUT} ($(stat -c%s "${OUT}") bytes)"
echo "---"
mkimage -l "${OUT}" | grep -E "^(FIT desc|Default|Config)"
echo ""
echo "Copy to SD card FAT partition root as 'boot.sd':"
echo "  cp ${OUT} /path/to/sdcard/boot.sd"
