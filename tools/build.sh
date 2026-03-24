#!/usr/bin/env bash
# Aeglos OS — Build and Run
# Usage:
#   ./tools/build.sh          Build and run in QEMU (serial only)
#   ./tools/build.sh build    Build only
#   ./tools/build.sh run      Run only (serial, assumes already built)
#   ./tools/build.sh display  Build and run with framebuffer display

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_DIR="$PROJECT_ROOT/kernel"
TARGET_JSON="$KERNEL_DIR/aarch64-unknown-none.json"
KERNEL_ELF="$PROJECT_ROOT/target/aarch64-unknown-none/release/aeglos"
KERNEL_BIN="$PROJECT_ROOT/target/aarch64-unknown-none/release/aeglos.bin"

# Use nightly toolchain
NIGHTLY="$HOME/.rustup/toolchains/nightly-aarch64-apple-darwin"
OBJCOPY="$NIGHTLY/lib/rustlib/aarch64-apple-darwin/bin/llvm-objcopy"
export PATH="$NIGHTLY/bin:$PATH"

MODEL_URL="https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf"
FAST_MODEL_URL="https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf"

# ── drive.img layout ───────────────────────────────────────────────────────────
#
#   [    0 MB ..  512 MB )  FAT32 partition  — ash + future userspace ELFs
#   [  512 MB .. ~5632 MB)  Qwen3-8B model (main inference, Q4_K_M, ~5.03 GB)
#   [ 6144 MB .. ~6544 MB)  Qwen3-0.6B model (fast inference, Q4_K_M, ~397 MB)
#   [ 7168 MB .. 7178 MB )  Semantic store (Ithildin, 10 MB = 20480 sectors)
#
# FAT32 is rebuilt on every invocation so freshly compiled binaries appear on
# disk without the user needing to manually re-format.  Model dd uses
# conv=notrunc so only the target region is touched.
# ──────────────────────────────────────────────────────────────────────────────

FAT32_MB=512        # FAT32 partition size in MiB
MODEL_SEEK=512      # dd seek in MiB for Qwen3-8B (main model)
FAST_MODEL_SEEK=6144  # dd seek in MiB for Qwen3-0.6B (fast model, 6 GB)

# Create/refresh the FAT32 partition image on macOS using hdiutil.
_fat32_macos() {
    local DRIVE="$1"
    local WORK
    WORK=$(mktemp -d)

    echo "[fat32] Building ${FAT32_MB} MB FAT32 partition (macOS)..."
    hdiutil create -megabytes "$FAT32_MB" -fs "MS-DOS FAT32" -volname "AEGLOS" \
        -layout NONE "$WORK/fs" -quiet

    mkdir -p "$WORK/mnt"
    hdiutil attach "$WORK/fs.dmg" -mountpoint "$WORK/mnt" -nobrowse -quiet

    # Copy compiled userspace ELF binaries to the root of the volume.
    local ASH="$PROJECT_ROOT/target/aarch64-unknown-none/release/ash"
    if [ -f "$ASH" ]; then
        cp "$ASH" "$WORK/mnt/ash"
        echo "[fat32]   /ash  ($(du -h "$ASH" | cut -f1))"
    fi

    local INSTALLER="$PROJECT_ROOT/target/aarch64-unknown-none/release/installer"
    if [ -f "$INSTALLER" ]; then
        cp "$INSTALLER" "$WORK/mnt/installer"
        echo "[fat32]   /installer  ($(du -h "$INSTALLER" | cut -f1))"
    fi

    hdiutil detach "$WORK/mnt" -quiet

    # Convert .dmg → flat raw image, then overwrite the FAT32 region of drive.img.
    hdiutil convert "$WORK/fs.dmg" -format UDTO -o "$WORK/fs" -quiet 2>/dev/null
    dd if="$WORK/fs.cdr" of="$DRIVE" bs=1M count="$FAT32_MB" conv=notrunc 2>/dev/null

    rm -rf "$WORK"
    echo "[fat32] FAT32 partition written to drive.img[0..${FAT32_MB} MB]"
}

# Create/refresh the FAT32 partition image on Linux using mkfs.fat + mtools.
_fat32_linux() {
    local DRIVE="$1"
    local WORK
    WORK=$(mktemp -d)

    if ! command -v mkfs.fat &>/dev/null; then
        echo "[fat32] WARN: mkfs.fat not found — FAT32 partition skipped."
        echo "             Install: apt install dosfstools"
        rm -rf "$WORK"
        return
    fi

    echo "[fat32] Building ${FAT32_MB} MB FAT32 partition (Linux)..."
    dd if=/dev/zero of="$WORK/fs.img" bs=1M count="$FAT32_MB" 2>/dev/null
    mkfs.fat -F 32 -n AEGLOS "$WORK/fs.img" >/dev/null 2>&1

    if command -v mcopy &>/dev/null; then
        local ASH="$PROJECT_ROOT/target/aarch64-unknown-none/release/ash"
        if [ -f "$ASH" ]; then
            mcopy -i "$WORK/fs.img" "$ASH" ::ash
            echo "[fat32]   /ash"
        fi

        local INSTALLER="$PROJECT_ROOT/target/aarch64-unknown-none/release/installer"
        if [ -f "$INSTALLER" ]; then
            mcopy -i "$WORK/fs.img" "$INSTALLER" ::installer
            echo "[fat32]   /installer"
        fi
    else
        echo "[fat32] INFO: mtools not found — binaries not copied to FAT32."
        echo "             Install: apt install mtools"
    fi

    dd if="$WORK/fs.img" of="$DRIVE" bs=1M count="$FAT32_MB" conv=notrunc 2>/dev/null
    rm -rf "$WORK"
    echo "[fat32] FAT32 partition written to drive.img[0..${FAT32_MB} MB]"
}

# Prepare drive.img: ensure it exists, write fresh FAT32, embed model.
setup_drive() {
    local DRIVE="$PROJECT_ROOT/drive.img"

    # 1. Create 8 GB blank image if it doesn't exist or is too small.
    local MIN_BYTES=$(( 8192 * 1024 * 1024 ))
    local ACTUAL_BYTES=0
    [ -f "$DRIVE" ] && ACTUAL_BYTES=$(wc -c < "$DRIVE" | tr -d ' ')
    if [ "$ACTUAL_BYTES" -lt "$MIN_BYTES" ]; then
        echo "[drive] Creating 8 GB drive.img (this takes a moment)..."
        dd if=/dev/zero of="$DRIVE" bs=1M count=8192 2>/dev/null
    fi

    # 2. Rebuild the FAT32 partition at [0..512 MB].
    if [ "$(uname -s)" = "Darwin" ]; then
        _fat32_macos "$DRIVE"
    else
        _fat32_linux "$DRIVE"
    fi

    # 3. Ensure the main Qwen3-8B model is present, then embed at [512 MB..].
    if [ ! -f "$PROJECT_ROOT/model.gguf" ]; then
        echo "[drive] Downloading Qwen3-8B-Q4_K_M model (~5 GB)..."
        curl -L -o "$PROJECT_ROOT/model.gguf" "$MODEL_URL"
    fi
    echo "[drive] Embedding main model at +${MODEL_SEEK} MB..."
    dd if="$PROJECT_ROOT/model.gguf" of="$DRIVE" bs=1M seek="$MODEL_SEEK" conv=notrunc 2>/dev/null

    # 4. Ensure the fast Qwen3-0.6B model is present, then embed at [6144 MB..].
    if [ ! -f "$PROJECT_ROOT/model_fast.gguf" ]; then
        echo "[drive] Downloading Qwen3-0.6B-Q4_K_M fast model (~397 MB)..."
        curl -L -o "$PROJECT_ROOT/model_fast.gguf" "$FAST_MODEL_URL"
    fi
    echo "[drive] Embedding fast model at +${FAST_MODEL_SEEK} MB..."
    dd if="$PROJECT_ROOT/model_fast.gguf" of="$DRIVE" bs=1M seek="$FAST_MODEL_SEEK" conv=notrunc 2>/dev/null

    echo "[drive] drive.img ready."
}

# ── Userspace: Ash EL0 shell ──────────────────────────────────────────────────

build_ash() {
    echo "[build] Building Ash EL0 userspace binary..."
    HOME=/tmp cargo build \
        --manifest-path "$PROJECT_ROOT/userspace/ash/Cargo.toml" \
        --target "$TARGET_JSON" \
        --release \
        -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem \
        -Zjson-target-spec \
        --target-dir "$PROJECT_ROOT/target"
    echo "[build] Ash ELF: $PROJECT_ROOT/target/aarch64-unknown-none/release/ash"
}

build_installer() {
    echo "[build] Building Installer EL0 userspace binary..."
    HOME=/tmp cargo build \
        --manifest-path "$PROJECT_ROOT/userspace/installer/Cargo.toml" \
        --target "$TARGET_JSON" \
        --release \
        -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem \
        -Zjson-target-spec \
        --target-dir "$PROJECT_ROOT/target"
    echo "[build] Installer ELF: $PROJECT_ROOT/target/aarch64-unknown-none/release/installer"
}

# ── Kernel ────────────────────────────────────────────────────────────────────

build() {
    # Userspace ELFs must be compiled first
    build_ash
    build_installer

    echo "[build] Building Aeglos kernel..."

    # HOME override works around ~/.config/git/ permission issues with cargo
    HOME=/tmp cargo build \
        --manifest-path "$KERNEL_DIR/Cargo.toml" \
        --target "$TARGET_JSON" \
        --release \
        -Zbuild-std=core,alloc \
        -Zbuild-std-features=compiler-builtins-mem \
        -Zjson-target-spec \
        --target-dir "$PROJECT_ROOT/target"

    echo "[build] Creating flat binary..."
    "$OBJCOPY" --strip-all -O binary "$KERNEL_ELF" "$KERNEL_BIN"

    echo "[build] Done. Kernel binary: $KERNEL_BIN ($(du -h "$KERNEL_BIN" | cut -f1))"
}

# ── Rust UEFI bootloader (used on macOS where grub-mkstandalone is unavailable)

build_bootloader() {
    echo "[iso] Building Rust UEFI bootloader (aarch64-unknown-uefi)..."
    rustup target add aarch64-unknown-uefi 2>/dev/null || true
    HOME=/tmp cargo build \
        --manifest-path "$PROJECT_ROOT/userspace/bootloader/Cargo.toml" \
        --target aarch64-unknown-uefi \
        --release \
        --target-dir "$PROJECT_ROOT/target"
    local EFI="$PROJECT_ROOT/target/aarch64-unknown-uefi/release/bootloader.efi"
    echo "[iso] Bootloader EFI: $EFI ($(du -h "$EFI" | cut -f1))"
}

# ── GRUB-based ISO (Linux / systems with grub-efi-arm64-bin installed) ────────

_iso_grub() {
    local ISO_OUT="$1"
    local ISO_ROOT="$PROJECT_ROOT/iso_root"

    rm -rf "$ISO_ROOT"
    mkdir -p "$ISO_ROOT/boot/grub" "$ISO_ROOT/EFI/BOOT"

    cp "$KERNEL_BIN" "$ISO_ROOT/boot/aeglos.bin"
    cp "$(dirname "$0")/grub.cfg" "$ISO_ROOT/boot/grub/grub.cfg"

    echo "[iso] Building GRUB arm64-efi EFI image..."
    grub-mkstandalone \
        --format=arm64-efi \
        --output="$ISO_ROOT/EFI/BOOT/BOOTAA64.EFI" \
        --modules="part_gpt part_msdos fat iso9660 normal linux boot" \
        "boot/grub/grub.cfg=$(dirname "$0")/grub.cfg"

    xorriso -as mkisofs \
        -iso-level 3 \
        -full-iso9660-filenames \
        -eltorito-alt-boot \
        -e EFI/BOOT/BOOTAA64.EFI \
        -no-emul-boot \
        --protective-msdos-label \
        -output "$ISO_OUT" \
        "$ISO_ROOT" 2>&1 | grep -v "^$"

    rm -rf "$ISO_ROOT"
}

# ── Native ISO (macOS: Rust UEFI bootloader + hdiutil + xorriso) ──────────────
#
# The Rust UEFI bootloader replaces GRUB. It reads \boot\aeglos.bin from its
# own FAT volume (the EFI partition embedded in the ISO) and jumps to it,
# passing the DTB pointer from the UEFI configuration table in x0.
#
# ISO structure:
#   efi.img  ← FAT12 image containing EFI/BOOT/BOOTAA64.EFI + boot/aeglos.bin
#
# UEFI firmware (OVMF) reads the El Torito catalog, finds efi.img, mounts it
# as a filesystem, loads EFI/BOOT/BOOTAA64.EFI, and hands off to the bootloader.

_iso_native() {
    local ISO_OUT="$1"
    local WORK
    WORK=$(mktemp -d)

    build_bootloader
    local BOOTLOADER_EFI="$PROJECT_ROOT/target/aarch64-unknown-uefi/release/bootloader.efi"

    echo "[iso] Creating EFI FAT12 partition image..."
    # 32 MB: bootloader (~500 KB) + kernel (~4 MB) + headroom for growth
    hdiutil create -megabytes 32 -fs "MS-DOS FAT12" -volname "AEGLOS" \
        -layout NONE "$WORK/efi_raw" -quiet

    mkdir -p "$WORK/mnt"
    hdiutil attach "$WORK/efi_raw.dmg" \
        -mountpoint "$WORK/mnt" -nobrowse -quiet

    mkdir -p "$WORK/mnt/EFI/BOOT" "$WORK/mnt/boot"
    cp "$BOOTLOADER_EFI" "$WORK/mnt/EFI/BOOT/BOOTAA64.EFI"
    cp "$KERNEL_BIN"     "$WORK/mnt/boot/aeglos.bin"

    hdiutil detach "$WORK/mnt" -quiet

    # Convert .dmg → raw flat image so xorriso can embed it
    hdiutil convert "$WORK/efi_raw.dmg" -format UDTO -o "$WORK/efi" -quiet 2>/dev/null
    mv "$WORK/efi.cdr" "$WORK/efi.img"

    echo "[iso] Creating ISO 9660 image with UEFI boot entry..."
    mkdir -p "$WORK/iso_root"
    cp "$WORK/efi.img" "$WORK/iso_root/efi.img"

    xorriso -as mkisofs \
        -iso-level 3 \
        -full-iso9660-filenames \
        -eltorito-alt-boot \
        -e efi.img \
        -no-emul-boot \
        --protective-msdos-label \
        -output "$ISO_OUT" \
        "$WORK/iso_root" 2>&1 | grep -v "^$"

    rm -rf "$WORK"
}

# ── Public iso() entrypoint ───────────────────────────────────────────────────

iso() {
    # Build a UEFI-bootable ISO image.
    #
    # On Linux (grub-efi-arm64-bin + xorriso available):  uses GRUB
    # On macOS (xorriso + hdiutil available):              uses our Rust bootloader
    #
    # The resulting aeglos.iso can be:
    #   - Booted in QEMU via: ./tools/build.sh run_iso
    #   - Written to USB:     sudo dd if=aeglos.iso of=/dev/sdX bs=4M

    if [ ! -f "$KERNEL_BIN" ]; then
        echo "[iso] Kernel binary not found, building first..."
        build
    fi

    if ! command -v xorriso &>/dev/null; then
        echo "[iso] ERROR: xorriso not found."
        echo "      macOS: brew install xorriso"
        echo "      Linux: apt install xorriso"
        exit 1
    fi

    ISO_OUT="$PROJECT_ROOT/aeglos.iso"

    if command -v grub-mkstandalone &>/dev/null; then
        echo "[iso] GRUB found — using GRUB arm64-efi pipeline..."
        _iso_grub "$ISO_OUT"
    else
        echo "[iso] No GRUB — using Rust UEFI bootloader pipeline (macOS-native)..."
        _iso_native "$ISO_OUT"
    fi

    echo "[iso] Done: $ISO_OUT ($(du -h "$ISO_OUT" | cut -f1))"
}

run_iso() {
    # Boot aeglos.iso in QEMU using OVMF (UEFI firmware bundled with QEMU).
    # This is the closest emulation of how the ISO would boot on real hardware.
    #
    # OVMF is installed alongside QEMU:
    #   macOS: brew install qemu   (OVMF at $(brew --prefix qemu)/share/qemu/)
    #   Linux: apt install qemu-system-arm ovmf

    if [ ! -f "$PROJECT_ROOT/aeglos.iso" ]; then
        echo "[run_iso] aeglos.iso not found, building first..."
        iso
    fi

    # Locate OVMF firmware (UEFI for AArch64)
    OVMF=""
    for candidate in \
        "$(brew --prefix qemu 2>/dev/null)/share/qemu/edk2-aarch64-code.fd" \
        "/usr/share/qemu/edk2-aarch64-code.fd" \
        "/usr/share/OVMF/OVMF_CODE_4M.fd" \
        "/usr/share/edk2-ovmf/OVMF_CODE.fd"; do
        if [ -f "$candidate" ]; then
            OVMF="$candidate"
            break
        fi
    done

    if [ -z "$OVMF" ]; then
        echo "[run_iso] ERROR: OVMF AArch64 firmware not found."
        echo "          macOS: brew install qemu"
        echo "          Linux: apt install ovmf"
        exit 1
    fi

    echo "[run_iso] Using OVMF: $OVMF"

    setup_drive

    CPU_ARGS="-cpu cortex-a72"
    if [ "$(uname -m)" = "arm64" ] && [ "$(uname -s)" = "Darwin" ]; then
        echo "[run_iso] Apple Silicon: enabling HVF acceleration..."
        CPU_ARGS="-cpu host -accel hvf"
    fi

    # drive.img holds the Qwen model at the 512MB offset.
    # It must be the FIRST VirtIO block device (0x0A000000) so the kernel's
    # block driver finds it there. The ISO is the second device (0x0A000200)
    # and is only needed by GRUB/UEFI for loading — the kernel ignores it.
    echo "[run_iso] Booting aeglos.iso via UEFI (Ctrl-A X to exit)..."
    qemu-system-aarch64 \
        -machine virt \
        $CPU_ARGS \
        -m 16384M \
        -nographic \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
        -drive if=none,file="$PROJECT_ROOT/drive.img",id=hd0,format=raw \
        -device virtio-blk-device,drive=hd0 \
        -drive if=none,file="$PROJECT_ROOT/aeglos.iso",id=cdrom0,format=raw,readonly=on \
        -device virtio-blk-device,drive=cdrom0 \
        -netdev user,id=net0 \
        -device virtio-net-device,netdev=net0
}

run() {
    if [ ! -f "$KERNEL_BIN" ]; then
        echo "[run] Kernel binary not found, building first..."
        build
    fi

    setup_drive

    CPU_ARGS="-cpu cortex-a72"
    if [ "$(uname -m)" = "arm64" ] && [ "$(uname -s)" = "Darwin" ]; then
        echo "[run] Apple Silicon detected. Enabling direct hardware virtualization (HVF)..."
        CPU_ARGS="-cpu host -accel hvf"
    fi

    # Start a local HTTP test server on port 19999 so the in-kernel TCP test
    # can fetch http://10.0.2.2:19999/ via SLIRP's host-forward.
    # Port 19999 forwards host:19999 → VM port 19999, but we connect OUT from
    # the VM to 10.0.2.2:19999 which SLIRP maps to localhost:19999.
    TEST_HTTP_PID=""
    if command -v python3 &>/dev/null; then
        python3 -m http.server 19999 --directory /tmp >/dev/null 2>&1 &
        TEST_HTTP_PID=$!
        echo "[run] Local HTTP test server started on 127.0.0.1:19999 (PID $TEST_HTTP_PID)"
    fi

    echo "[run] Starting QEMU (Ctrl-A X to exit)..."
    qemu-system-aarch64 \
        -machine virt \
        $CPU_ARGS \
        -m 16384M \
        -nographic \
        -drive if=none,file="$PROJECT_ROOT/drive.img",id=hd0,format=raw \
        -device virtio-blk-device,drive=hd0 \
        -netdev "user,id=net0" \
        -device virtio-net-device,netdev=net0 \
        -kernel "$KERNEL_BIN"

    # Clean up HTTP test server
    if [ -n "$TEST_HTTP_PID" ]; then
        kill "$TEST_HTTP_PID" 2>/dev/null
        echo "[run] Local HTTP test server stopped"
    fi
}

display() {
    if [ ! -f "$KERNEL_BIN" ]; then
        echo "[display] Kernel binary not found, building first..."
        build
    fi

    setup_drive

    CPU_ARGS="-cpu cortex-a72"
    if [ "$(uname -m)" = "arm64" ] && [ "$(uname -s)" = "Darwin" ]; then
        echo "[display] Apple Silicon detected. Enabling direct hardware virtualization (HVF)..."
        CPU_ARGS="-cpu host -accel hvf"
    fi

    # Start a local HTTP test server so fetch/curl/http commands work
    TEST_HTTP_PID=""
    if command -v python3 &>/dev/null; then
        python3 -m http.server 19999 --directory /tmp >/dev/null 2>&1 &
        TEST_HTTP_PID=$!
        echo "[display] Local HTTP test server started on 127.0.0.1:19999 (PID $TEST_HTTP_PID)"
    fi

    # Audio device flags (platform-specific backend)
    AUDIO_ARGS=""
    if [ "$(uname -s)" = "Darwin" ]; then
        AUDIO_ARGS="-audiodev coreaudio,id=snd0 -device intel-hda,id=hda -device hda-duplex,bus=hda.0,audiodev=snd0"
    else
        # Linux: try PulseAudio first, fall back to SDL
        if command -v pactl &>/dev/null; then
            AUDIO_ARGS="-audiodev pa,id=snd0 -device intel-hda,id=hda -device hda-duplex,bus=hda.0,audiodev=snd0"
        else
            AUDIO_ARGS="-audiodev sdl,id=snd0 -device intel-hda,id=hda -device hda-duplex,bus=hda.0,audiodev=snd0"
        fi
    fi

    echo "[display] Starting QEMU with virtio-gpu display..."
    qemu-system-aarch64 \
        -machine virt \
        $CPU_ARGS \
        -m 16384M \
        -device virtio-gpu-device \
        -device virtio-keyboard-device \
        -device virtio-tablet-device \
        -serial stdio \
        -drive if=none,file="$PROJECT_ROOT/drive.img",id=hd0,format=raw \
        -device virtio-blk-device,drive=hd0 \
        -netdev user,id=net0 \
        -device virtio-net-device,netdev=net0 \
        $AUDIO_ARGS \
        -kernel "$KERNEL_BIN"

    if [ -n "$TEST_HTTP_PID" ]; then
        kill "$TEST_HTTP_PID" 2>/dev/null
        echo "[display] Local HTTP test server stopped"
    fi
}

utm() {
    # Creates Aeglos.utm — a UTM VM bundle for macOS.
    # UTM (https://mac.getutm.app) is the standard QEMU frontend for Mac.
    # Double-click Aeglos.utm to launch; no QEMU knowledge needed.
    #
    # drive.img is symlinked into the bundle (not copied) to avoid duplicating 8 GB.
    # For distribution: replace the symlink with a copy — see note at end.

    if [ "$(uname -s)" != "Darwin" ]; then
        echo "[utm] UTM bundles are macOS-only."
        exit 1
    fi

    if [ ! -f "$KERNEL_BIN" ]; then
        echo "[utm] Kernel binary not found, building first..."
        build
    fi

    setup_drive

    local BUNDLE="$PROJECT_ROOT/Aeglos.utm"
    local DATA="$BUNDLE/Data"
    local DRIVE_UUID
    DRIVE_UUID=$(uuidgen | tr '[:upper:]' '[:lower:]')
    local VM_UUID
    VM_UUID=$(uuidgen | tr '[:upper:]' '[:lower:]')

    echo "[utm] Creating $BUNDLE ..."
    rm -rf "$BUNDLE"
    mkdir -p "$DATA"

    # Kernel binary (~4-5 MB, just copy it)
    cp "$KERNEL_BIN" "$DATA/aeglos.bin"
    echo "[utm]   kernel : Data/aeglos.bin ($(du -h "$DATA/aeglos.bin" | cut -f1))"

    # Drive image — symlink to avoid duplicating 8 GB on disk
    local DRIVE_ABS
    DRIVE_ABS="$(cd "$PROJECT_ROOT" && pwd)/drive.img"
    ln -sf "$DRIVE_ABS" "$DATA/${DRIVE_UUID}.img"
    echo "[utm]   drive  : Data/${DRIVE_UUID}.img → drive.img (symlink)"

    # config.plist — UTM 4.x QEMU backend
    cat > "$BUNDLE/config.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Backend</key>
	<string>qemu</string>
	<key>ConfigurationVersion</key>
	<integer>4</integer>
	<key>Display</key>
	<array>
		<dict>
			<key>Dynamic</key>
			<false/>
			<key>Hardware</key>
			<string>virtio-gpu-device</string>
			<key>UpscalerMode</key>
			<string>nearest</string>
		</dict>
	</array>
	<key>Drive</key>
	<array>
		<dict>
			<key>Identifier</key>
			<string>${DRIVE_UUID}</string>
			<key>ImageType</key>
			<string>disk</string>
			<key>Interface</key>
			<string>virtio</string>
			<key>ReadOnly</key>
			<false/>
			<key>Removable</key>
			<false/>
		</dict>
	</array>
	<key>Information</key>
	<dict>
		<key>IconCustom</key>
		<false/>
		<key>Name</key>
		<string>Aeglos OS</string>
		<key>UUID</key>
		<string>${VM_UUID}</string>
	</dict>
	<key>Input</key>
	<dict>
		<key>GameController</key>
		<false/>
		<key>UsbBusSupport</key>
		<false/>
	</dict>
	<key>Network</key>
	<array>
		<dict>
			<key>Hardware</key>
			<string>virtio-net-device</string>
			<key>Mode</key>
			<string>emulated</string>
		</dict>
	</array>
	<key>QEMU</key>
	<dict>
		<key>AdditionalArguments</key>
		<array>
			<dict><key>string</key><string>-kernel</string></dict>
			<dict><key>string</key><string>${DATA}/aeglos.bin</string></dict>
			<dict><key>string</key><string>-device</string></dict>
			<dict><key>string</key><string>virtio-keyboard-device</string></dict>
			<dict><key>string</key><string>-device</string></dict>
			<dict><key>string</key><string>virtio-tablet-device</string></dict>
			<dict><key>string</key><string>-audiodev</string></dict>
			<dict><key>string</key><string>coreaudio,id=snd0</string></dict>
			<dict><key>string</key><string>-device</string></dict>
			<dict><key>string</key><string>intel-hda,id=hda</string></dict>
			<dict><key>string</key><string>-device</string></dict>
			<dict><key>string</key><string>hda-duplex,bus=hda.0,audiodev=snd0</string></dict>
		</array>
		<key>HasHypervisor</key>
		<true/>
	</dict>
	<key>Sharing</key>
	<dict>
		<key>DirectoryReadOnly</key>
		<false/>
		<key>DirectoryShares</key>
		<array/>
		<key>PasteboardSharing</key>
		<true/>
	</dict>
	<key>Sound</key>
	<array/>
	<key>System</key>
	<dict>
		<key>Architecture</key>
		<string>aarch64</string>
		<key>CPU</key>
		<string>default</string>
		<key>CPUCount</key>
		<integer>4</integer>
		<key>ForceMulticore</key>
		<false/>
		<key>JitCacheSize</key>
		<integer>0</integer>
		<key>MemorySize</key>
		<integer>16384</integer>
		<key>Target</key>
		<string>virt</string>
	</dict>
</dict>
</plist>
PLIST

    echo "[utm] Bundle created: $BUNDLE"
    echo ""
    echo "[utm] ▶  Double-click Aeglos.utm to open in UTM (https://mac.getutm.app)"
    echo ""
    echo "[utm] NOTE: drive.img is symlinked — bundle must stay in this directory."
    echo "[utm] For distribution (portable bundle), replace the symlink with a copy:"
    echo "        cp \"$DRIVE_ABS\" \"$DATA/${DRIVE_UUID}.img\""
}

case "${1:-all}" in
    build)    build ;;
    run)      run ;;
    display)  build && display ;;
    all)      build && run ;;
    iso)      iso ;;
    run_iso)  run_iso ;;
    iso_run)  iso && run_iso ;;
    utm)      build && utm ;;
    *)
        echo "Usage: $0 {build|run|display|all|iso|run_iso|iso_run|utm}"
        exit 1
        ;;
esac
