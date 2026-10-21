#!/usr/bin/env bash
set -euo pipefail

image_dir=${1:?usage: qemu-smoke.sh IMAGE_DIR [SECONDS]}
seconds=${2:-45}
log=$(mktemp)
trap 'rm -f "$log"' EXIT

for file in vmlinuz initramfs.img rootfs.img; do
    test -r "$image_dir/$file" || { echo "missing $image_dir/$file" >&2; exit 2; }
done

set +e
timeout --preserve-status "$seconds" qemu-system-x86_64 \
    -accel tcg -cpu max -m 2048 -smp 2 \
    -kernel "$image_dir/vmlinuz" -initrd "$image_dir/initramfs.img" \
    -append 'root=LABEL=COCONUT rw console=ttyS0 selinux=0' \
    -drive "file=$image_dir/rootfs.img,format=raw,if=virtio,snapshot=on" \
    -device virtio-vga -display none -serial "file:$log" -no-reboot
status=$?
set -e
if test "$status" -ne 124; then
    cat "$log" >&2
    exit "$status"
fi
grep -q 'Linux version' "$log" || { cat "$log" >&2; exit 1; }
grep -q 'Run /init as init process' "$log" || { cat "$log" >&2; exit 1; }
