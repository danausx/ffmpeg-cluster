#!/bin/bash

# Colors for better UI
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print in color
print_color() {
    printf "${!1}%s${NC}\n" "$2"
}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Function to check and install rust target
check_and_install_target() {
    local target=$1
    if ! rustup target list | grep -q "$target installed"; then
        print_color "YELLOW" "Installing Rust target $target..."
        rustup target add "$target" || exit 1
    fi
}

# Function to download and extract FFmpeg
download_ffmpeg() {
    local os=$1
    local arch=$2
    print_color "BLUE" "Downloading FFmpeg for $os..."
    
    mkdir -p build/bin

    case "$os" in
        "macos-x86_64"|"macos-aarch64")
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip" -o ffmpeg.zip
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip" -o ffprobe.zip
            
            unzip ffmpeg.zip -d build/bin/
            unzip ffprobe.zip -d build/bin/
            chmod +x build/bin/ffmpeg
            chmod +x build/bin/ffprobe
            rm ffmpeg.zip ffprobe.zip
            ;;

        "linux-x86_64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-amd64-static/ffmpeg build/bin/
            mv ffmpeg-*-amd64-static/ffprobe build/bin/
            rm -rf ffmpeg-*-amd64-static
            rm ffmpeg.tar.xz
            chmod +x build/bin/ffmpeg
            chmod +x build/bin/ffprobe
            ;;

        "linux-aarch64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-arm64-static/ffmpeg build/bin/
            mv ffmpeg-*-arm64-static/ffprobe build/bin/
            rm -rf ffmpeg-*-arm64-static
            rm ffmpeg.tar.xz
            chmod +x build/bin/ffmpeg
            chmod +x build/bin/ffprobe
            ;;

        "windows-x86_64")
            curl -L "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -o ffmpeg.zip
            unzip ffmpeg.zip
            mv ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe build/bin/
            mv ffmpeg-master-latest-win64-gpl/bin/ffprobe.exe build/bin/
            rm -rf ffmpeg-master-latest-win64-gpl
            rm ffmpeg.zip
            ;;
    esac
}

# Check for required tools
print_color "BLUE" "Checking required tools..."
MISSING_TOOLS=()

for tool in "cargo" "curl" "unzip" "tar"; do
    if ! command_exists "$tool"; then
        MISSING_TOOLS+=("$tool")
    fi
done

if [ ${#MISSING_TOOLS[@]} -ne 0 ]; then
    print_color "RED" "Missing required tools: ${MISSING_TOOLS[*]}"
    print_color "YELLOW" "Please install them and try again."
    exit 1
fi

# Target selection menu
echo 
print_color "GREEN" "Select target platform:"
echo "1) macOS (x86_64)"
echo "2) macOS (ARM64/M1/M2)"
echo "3) Linux (x86_64)"
echo "4) Linux (ARM64)"
echo "5) Windows (x86_64)"
echo "6) Quit"
echo 

read -p "Enter your choice (1-6): " choice

case $choice in
    1)
        target="macos-x86_64"
        rust_target="x86_64-apple-darwin"
        ;;
    2)
        target="macos-aarch64"
        rust_target="aarch64-apple-darwin"
        ;;
    3)
        target="linux-x86_64"
        rust_target="x86_64-unknown-linux-gnu"
        ;;
    4)
        target="linux-aarch64"
        rust_target="aarch64-unknown-linux-gnu"
        ;;
    5)
        target="windows-x86_64"
        rust_target="x86_64-pc-windows-msvc"
        ;;
    6)
        print_color "YELLOW" "Build cancelled."
        exit 0
        ;;
    *)
        print_color "RED" "Invalid choice"
        exit 1
        ;;
esac

# Clean old build and release files
rm -rf build release
mkdir -p build/bin

# Install Rust target if needed
check_and_install_target "$rust_target"

# Download and extract FFmpeg
download_ffmpeg "$target"

# Build the client
print_color "BLUE" "Building client for $target..."
cargo build --release --target "$rust_target"

# Copy the binary to build directory
if [[ $target == windows* ]]; then
    cp "target/$rust_target/release/ffmpeg-cluster-client.exe" build/bin/
    binary_name="ffmpeg-cluster-client.exe"
else
    cp "target/$rust_target/release/ffmpeg-cluster-client" build/bin/
    binary_name="ffmpeg-cluster-client"
    chmod +x "build/bin/$binary_name"
fi

# Verify all required files are present
if [ ! -f "build/bin/$binary_name" ]; then
    print_color "RED" "Error: Client binary not found!"
    exit 1
fi

if [[ $target == windows* ]]; then
    if [ ! -f "build/bin/ffmpeg.exe" ] || [ ! -f "build/bin/ffprobe.exe" ]; then
        print_color "RED" "Error: FFmpeg binaries not found!"
        exit 1
    fi
else
    if [ ! -f "build/bin/ffmpeg" ] || [ ! -f "build/bin/ffprobe" ]; then
        print_color "RED" "Error: FFmpeg binaries not found!"
        exit 1
    fi
fi

# Create the release package
mkdir -p release
if [[ $target == windows* ]]; then
    print_color "BLUE" "Creating Windows ZIP archive..."
    cd build && zip -r "../release/ffmpeg-cluster-client-$target.zip" * && cd ..
else
    print_color "BLUE" "Creating tar.gz archive..."
    cd build && tar czf "../release/ffmpeg-cluster-client-$target.tar.gz" * && cd ..
fi

print_color "GREEN" "Build complete!"
print_color "BLUE" "Release package created in: release/ffmpeg-cluster-client-$target.$(if [[ $target == windows* ]]; then echo "zip"; else echo "tar.gz"; fi)"

# List the contents of the archive
echo
print_color "BLUE" "Archive contents:"
if [[ $target == windows* ]]; then
    unzip -l "release/ffmpeg-cluster-client-$target.zip"
else
    tar tvf "release/ffmpeg-cluster-client-$target.tar.gz"
fi