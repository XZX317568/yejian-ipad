#!/usr/bin/env bash
# Build the Rust static library and place it where Yejian.xcodeproj looks.
#
# On a Mac or a GitHub-hosted macOS runner. --ipa produces a device build
# for a sideloading tool to sign on the user's device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/rust-reader/Cargo.toml"
IOS_DIR="$ROOT/rust-reader/platform/ios"

PACKAGE="yejian"
LIB_NAME=""
PROFILE="debug"
DEST="device"
RUN_XCODEBUILD=0
MAKE_IPA=0

usage() {
    cat <<'EOF'
用法：tools/build-rust-ios.sh [选项]

  --device          构建真机 aarch64-apple-ios 静态库（默认）
  --sim, --simulator
                    构建本机架构的 iOS 模拟器静态库
  --release         使用 release profile
  --package <名称>  Cargo 包名，默认 yejian
  --lib-name <名称> 静态库名（不含 lib 和 .a），默认由包名推导
  --xcodebuild      静态库就位后运行 xcodebuild 编译检查（不签名）
  --ipa             用 Xcode 编译真机版并导出供侧载的 IPA
  -h, --help        显示本帮助
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --device)
            DEST="device"
            ;;
        --sim|--simulator)
            DEST="sim"
            ;;
        --release)
            PROFILE="release"
            ;;
        --package)
            PACKAGE="${2:-}"
            shift
            ;;
        --lib-name)
            LIB_NAME="${2:-}"
            shift
            ;;
        --xcodebuild)
            RUN_XCODEBUILD=1
            ;;
        --ipa)
            MAKE_IPA=1
            RUN_XCODEBUILD=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "未知参数：$1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [[ "$MAKE_IPA" == "1" && "$DEST" != "device" ]]; then
    echo "iPad 安装包需要真机目标，请使用 --device --ipa。" >&2
    exit 2
fi

if [[ -z "$PACKAGE" ]]; then
    echo "包名不能为空。" >&2
    exit 2
fi
if [[ -z "$LIB_NAME" ]]; then
    LIB_NAME="${PACKAGE//-/_}"
fi

if [[ ! -f "$MANIFEST" ]]; then
    echo "找不到 Rust 工程：$MANIFEST" >&2
    exit 1
fi

if [[ "$DEST" == "device" ]]; then
    TRIPLE="aarch64-apple-ios"
    PLATFORM_DIR="iphoneos"
    DESTINATION="generic/platform=iOS"
else
    case "$(uname -m)" in
        arm64)
            TRIPLE="aarch64-apple-ios-sim"
            ;;
        x86_64)
            TRIPLE="x86_64-apple-ios"
            ;;
        *)
            echo "不支持的模拟器宿主架构：$(uname -m)" >&2
            exit 1
            ;;
    esac
    PLATFORM_DIR="iphonesimulator"
    DESTINATION="generic/platform=iOS Simulator"
fi

if command -v rustup >/dev/null 2>&1; then
    if ! rustup target list --installed | grep -qx "$TRIPLE"; then
        echo "缺少 Rust 目标 $TRIPLE。" >&2
        echo "请先运行：rustup target add $TRIPLE" >&2
        exit 1
    fi
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/rust-reader/target}"
mkdir -p "$TARGET_DIR"
TARGET_DIR="$(cd "$TARGET_DIR" && pwd)"
RUST_PRODUCT_DIR="$TARGET_DIR/$TRIPLE/$PROFILE"
mkdir -p "$RUST_PRODUCT_DIR"
LINK_FLAGS="$RUST_PRODUCT_DIR/$LIB_NAME.native-static-libs.rsp"

# Xcode supplies the Swift half of the bridge at the final link. Asking
# Cargo for the Android cdylib as well would link it before Swift is present.
BUILD_ARGS=(rustc --manifest-path "$MANIFEST" --lib --crate-type staticlib
    -j "${CARGO_BUILD_JOBS:-1}" -p "$PACKAGE" --target "$TRIPLE" --target-dir "$TARGET_DIR")
if [[ "$PROFILE" == "release" ]]; then
    BUILD_ARGS+=(--release)
fi
if [[ "$MAKE_IPA" == "1" ]]; then
    BUILD_ARGS+=(--locked)
fi

echo "目标：$TRIPLE（$PROFILE）"
cargo "${BUILD_ARGS[@]}" -- --print "native-static-libs=$LINK_FLAGS"

LIB_SOURCE="$RUST_PRODUCT_DIR/lib$LIB_NAME.a"
if [[ ! -f "$LIB_SOURCE" ]]; then
    echo "没有生成 $LIB_SOURCE。" >&2
    echo "请确认包的 [lib] name 为 $LIB_NAME 且 crate-type 包含 staticlib。" >&2
    exit 1
fi

DEST_DIR="$IOS_DIR/Libs/$PLATFORM_DIR"
mkdir -p "$DEST_DIR"
cp -f "$LIB_SOURCE" "$DEST_DIR/lib$LIB_NAME.a"
cp -f "$LINK_FLAGS" "$DEST_DIR/native-static-libs.rsp"
echo "静态库：$DEST_DIR/lib$LIB_NAME.a"

if [[ "$RUN_XCODEBUILD" == "1" ]]; then
    CONFIGURATION="Debug"
    if [[ "$PROFILE" == "release" ]]; then
        CONFIGURATION="Release"
    fi
    XCODE_ARGS=(-project "$IOS_DIR/Yejian.xcodeproj" -scheme Yejian
        -configuration "$CONFIGURATION" -destination "$DESTINATION")
    if [[ "$MAKE_IPA" == "1" ]]; then
        XCODE_ARGS+=(-derivedDataPath "$TARGET_DIR/yejian-xcode" -sdk iphoneos)
    fi
    xcodebuild "${XCODE_ARGS[@]}" build CODE_SIGNING_ALLOWED=NO
fi

if [[ "$MAKE_IPA" == "1" ]]; then
    APP="$TARGET_DIR/yejian-xcode/Build/Products/$CONFIGURATION-iphoneos/Yejian.app"
    INFO="$APP/Info.plist"
    EXECUTABLE="$(plutil -extract CFBundleExecutable raw -o - "$INFO")"
    xcrun lipo "$APP/$EXECUTABLE" -verify_arch arm64
    python3 "$ROOT/tools/package-rust-ipad.py" "$APP" "$ROOT/artifacts/rust/ipad"
fi
