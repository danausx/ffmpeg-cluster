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

# Function to check and install cross if needed
check_cross() {
    if ! command_exists cross; then
        print_color "YELLOW" "Installing cross..."
        cargo install cross --git https://github.com/cross-rs/cross || {
            print_color "RED" "Failed to install cross"
            exit 1
        }
    fi
}

# Function to check if cargo is installed
check_cargo() {
    if ! command_exists cargo; then
        print_color "RED" "Rust's cargo is required. Please install from https://rustup.rs"
        exit 1
    fi
}

# Function to check Docker
check_docker() {
    if ! command_exists docker; then
        print_color "RED" "Docker is required for cross compilation. Please install Docker"
        exit 1
    fi
    
    if ! docker info >/dev/null 2>&1; then
        print_color "RED" "Docker is not running. Please start Docker"
        exit 1
    fi
}

# Function to download and extract FFmpeg
download_ffmpeg() {
    local os=$1
    print_color "BLUE" "Downloading FFmpeg for $os..."
    
    mkdir -p build/ffmpeg-cluster-client/bin

    case "$os" in
        "macos-universal2"|"macos-x86_64"|"macos-aarch64")
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip" -o ffmpeg.zip
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip" -o ffprobe.zip
            
            unzip -o ffmpeg.zip -d build/ffmpeg-cluster-client/bin/
            unzip -o ffprobe.zip -d build/ffmpeg-cluster-client/bin/
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            rm ffmpeg.zip ffprobe.zip
            ;;

        "linux-x86_64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-amd64-static/ffmpeg build/ffmpeg-cluster-client/bin/
            mv ffmpeg-*-amd64-static/ffprobe build/ffmpeg-cluster-client/bin/
            rm -rf ffmpeg-*-amd64-static
            rm ffmpeg.tar.xz
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            ;;

        "linux-aarch64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-arm64-static/ffmpeg build/ffmpeg-cluster-client/bin/
            mv ffmpeg-*-arm64-static/ffprobe build/ffmpeg-cluster-client/bin/
            rm -rf ffmpeg-*-arm64-static
            rm ffmpeg.tar.xz
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            ;;

        "windows-x86_64")
            curl -L "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -o ffmpeg.zip
            unzip -o ffmpeg.zip
            mv ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe build/ffmpeg-cluster-client/bin/
            mv ffmpeg-master-latest-win64-gpl/bin/ffprobe.exe build/ffmpeg-cluster-client/bin/
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
echo "1) macOS (Universal - Intel & Apple Silicon)"
echo "2) macOS (x86_64 - Intel only)"
echo "3) macOS (ARM64/M1/M2 only)"
echo "4) Linux (x86_64)"
echo "5) Linux (ARM64)"
echo "6) Windows (x86_64)"
echo "7) Current platform (native)"
echo "8) Quit"
echo 

read -p "Enter your choice (1-8): " choice

case $choice in
    1)
        target="macos-universal2"
        rust_target="universal2-apple-darwin"
        check_docker
        ;;
    2)
        target="macos-x86_64"
        rust_target="x86_64-apple-darwin"
        ;;
    3)
        target="macos-aarch64"
        rust_target="aarch64-apple-darwin"
        ;;
    4)
        target="linux-x86_64"
        rust_target="x86_64-unknown-linux-gnu"
        check_docker
        check_cross
        ;;
    5)
        target="linux-aarch64"
        rust_target="aarch64-unknown-linux-gnu"
        check_docker
        check_cross
        ;;
    6)
        target="windows-x86_64"
        rust_target="x86_64-pc-windows-gnu"
        check_cross
        ;;
    7)
        target="native"
        rust_target=""  # Will use default target
        ;;
    8)
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

# Download and extract FFmpeg
download_ffmpeg "$target"

# Build the client
print_color "BLUE" "Building client for $target..."

if [ "$target" = "native" ]; then
    cargo build --release
elif [[ "$target" == linux* ]] || [ "$target" = "windows-x86_64" ]; then
    # Use cross for Linux and Windows builds
    cross build --release --target "$rust_target"
else
    # Regular cargo for other targets
    cargo build --release --target "$rust_target"
fi

# Copy the binary to build directory
mkdir -p build/ffmpeg-cluster-client
if [[ $target == windows* ]]; then
    cp "target/$rust_target/release/ffmpeg-cluster-client.exe" build/ffmpeg-cluster-client/
    binary_name="ffmpeg-cluster-client.exe"
else
    if [ "$target" = "native" ]; then
        cp "target/release/ffmpeg-cluster-client" build/ffmpeg-cluster-client/
    else
        cp "target/$rust_target/release/ffmpeg-cluster-client" build/ffmpeg-cluster-client/
    fi
    binary_name="ffmpeg-cluster-client"
    chmod +x "build/ffmpeg-cluster-client/$binary_name"
fi

# Verify all required files are present
if [ ! -f "build/ffmpeg-cluster-client/$binary_name" ]; then
    print_color "RED" "Error: Client binary not found!"
    exit 1
fi

if [[ $target == windows* ]]; then
    if [ ! -f "build/ffmpeg-cluster-client/bin/ffmpeg.exe" ] || [ ! -f "build/ffmpeg-cluster-client/bin/ffprobe.exe" ]; then
        print_color "RED" "Error: FFmpeg binaries not found!"
        exit 1
    fi
else
    if [ ! -f "build/ffmpeg-cluster-client/bin/ffmpeg" ] || [ ! -f "build/ffmpeg-cluster-client/bin/ffprobe" ]; then
        print_color "RED" "Error: FFmpeg binaries not found!"
        exit 1
    fi
fi

# Create the release package
mkdir -p release
if [[ $target == windows* ]]; then
    print_color "BLUE" "Creating Windows ZIP archive..."
    cd build && zip -r "../release/ffmpeg-cluster-client-$target.zip" ffmpeg-cluster-client && cd ..
else
    print_color "BLUE" "Creating tar.gz archive..."
    cd build && tar czf "../release/ffmpeg-cluster-client-$target.tar.gz" ffmpeg-cluster-client && cd ..
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