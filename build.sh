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

# Print header
clear
print_color "BLUE" "===================================="
print_color "BLUE" "   FFmpeg Cluster Builder Script    "
print_color "BLUE" "===================================="
echo

# Check for required tools
print_color "BLUE" "Checking required tools..."
MISSING_TOOLS=()

# Check each tool and show status
for tool in "cargo" "curl" "unzip" "tar"; do
    printf "Checking for %s..." "$tool"
    if command_exists "$tool"; then
        print_color "GREEN" " found"
    else
        print_color "RED" " not found"
        MISSING_TOOLS+=("$tool")
    fi
done

if [ ${#MISSING_TOOLS[@]} -ne 0 ]; then
    echo
    print_color "RED" "Missing required tools: ${MISSING_TOOLS[*]}"
    print_color "YELLOW" "Please install them and try again."
    exit 1
fi

echo
print_color "GREEN" "All required tools are available."
echo

# Function to handle build errors
handle_build_error() {
    print_color "RED" "Build failed! See error message above."
    rm -rf build release
    exit 1
}

# Set error trap
trap 'handle_build_error' ERR

# Function to check and install cross if needed
check_cross() {
    printf "Checking for cross..."
    if ! command_exists cross; then
        print_color "YELLOW" " not found, installing..."
        cargo install cross --git https://github.com/cross-rs/cross || {
            print_color "RED" "Failed to install cross"
            exit 1
        }
    else
        print_color "GREEN" " found"
    fi
}

# Function to check Docker
check_docker() {
    printf "Checking for Docker..."
    if ! command_exists docker; then
        print_color "RED" " not found"
        print_color "RED" "Docker is required for cross compilation. Please install Docker"
        exit 1
    fi
    
    printf "Checking if Docker is running..."
    if ! docker info >/dev/null 2>&1; then
        print_color "RED" " not running"
        print_color "RED" "Docker is not running. Please start Docker"
        exit 1
    fi
    print_color "GREEN" " running"
}

# Show component selection menu
print_color "GREEN" "Select components to build:"
echo "1) Both (Client + Server) [default]"
echo "2) Client only"
echo "3) Server only"
echo
printf "Enter choice (1-3) [default: 1]: "
read -r component_choice
echo

# Map component choice
case $component_choice in
    "2")
        selected_component="1"  # Client only
        ;;
    "3")
        selected_component="2"  # Server only
        ;;
    *)
        selected_component="0"  # Both (default)
        ;;
esac

# Show platform selection menu
print_color "GREEN" "Select target platform:"
echo "1) macOS (Universal - Intel & Apple Silicon)"
echo "2) macOS (x86_64 - Intel only)"
echo "3) macOS (ARM64/M1/M2 only)"
echo "4) Linux (x86_64)"
echo "5) Linux (ARM64)"
echo "6) Windows (x86_64)"
echo "7) Current platform (native) [default]"
echo "8) Quit"
echo
printf "Enter choice (1-8) [default: 7]: "
read -r platform_choice
echo

# Map platform choice
case $platform_choice in
    "1")
        target="macos-universal2"
        rust_target="universal2-apple-darwin"
        check_docker
        ;;
    "2")
        target="macos-x86_64"
        rust_target="x86_64-apple-darwin"
        ;;
    "3")
        target="macos-aarch64"
        rust_target="aarch64-apple-darwin"
        ;;
    "4")
        target="linux-x86_64"
        rust_target="x86_64-unknown-linux-musl"
        check_docker
        check_cross
        ;;
    "5")
        target="linux-aarch64"
        rust_target="aarch64-unknown-linux-musl"
        check_docker
        check_cross
        ;;
    "6")
        target="windows-x86_64"
        rust_target="x86_64-pc-windows-musl"
        check_cross
        ;;
    "8")
        print_color "YELLOW" "Build cancelled."
        exit 0
        ;;
    *)
        target="native"
        rust_target=""
        ;;
esac

# Function to download and extract FFmpeg if not already present
download_ffmpeg() {
    local os=$1
    local ffmpeg_cache_dir="ffmpeg-cache/$os"
    
    # Check if FFmpeg binaries already exist in cache
    if [[ $os == windows* ]]; then
        if [ -f "$ffmpeg_cache_dir/ffmpeg.exe" ] && [ -f "$ffmpeg_cache_dir/ffprobe.exe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
            mkdir -p "build/ffmpeg-cluster-client/bin"
            cp "$ffmpeg_cache_dir/ffmpeg.exe" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe.exe" "build/ffmpeg-cluster-client/bin/"
            return
        fi
    else
        if [ -f "$ffmpeg_cache_dir/ffmpeg" ] && [ -f "$ffmpeg_cache_dir/ffprobe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
            mkdir -p "build/ffmpeg-cluster-client/bin"
            cp "$ffmpeg_cache_dir/ffmpeg" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe" "build/ffmpeg-cluster-client/bin/"
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            return
        fi
    fi

    print_color "BLUE" "Downloading FFmpeg for $os..."
    mkdir -p "$ffmpeg_cache_dir"
    mkdir -p "build/ffmpeg-cluster-client/bin"

    case "$os" in
        "macos-universal2"|"macos-x86_64"|"macos-aarch64")
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip" -o ffmpeg.zip
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip" -o ffprobe.zip
            
            unzip -o ffmpeg.zip -d "$ffmpeg_cache_dir/"
            unzip -o ffprobe.zip -d "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe" "build/ffmpeg-cluster-client/bin/"
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            rm ffmpeg.zip ffprobe.zip
            ;;

        "linux-x86_64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-amd64-static/ffmpeg "$ffmpeg_cache_dir/"
            mv ffmpeg-*-amd64-static/ffprobe "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe" "build/ffmpeg-cluster-client/bin/"
            rm -rf ffmpeg-*-amd64-static
            rm ffmpeg.tar.xz
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            ;;

        "linux-aarch64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            mv ffmpeg-*-arm64-static/ffmpeg "$ffmpeg_cache_dir/"
            mv ffmpeg-*-arm64-static/ffprobe "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe" "build/ffmpeg-cluster-client/bin/"
            rm -rf ffmpeg-*-arm64-static
            rm ffmpeg.tar.xz
            chmod +x build/ffmpeg-cluster-client/bin/ffmpeg
            chmod +x build/ffmpeg-cluster-client/bin/ffprobe
            ;;

        "windows-x86_64")
            curl -L "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -o ffmpeg.zip
            unzip -o ffmpeg.zip
            mv ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe "$ffmpeg_cache_dir/"
            mv ffmpeg-master-latest-win64-gpl/bin/ffprobe.exe "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg.exe" "build/ffmpeg-cluster-client/bin/"
            cp "$ffmpeg_cache_dir/ffprobe.exe" "build/ffmpeg-cluster-client/bin/"
            rm -rf ffmpeg-master-latest-win64-gpl
            rm ffmpeg.zip
            ;;
    esac
}

# Function to build component
build_component() {
    local component=$1
    local target=$2
    local rust_target=$3
    
    print_color "BLUE" "Building $component for $target..."
    
    # Create build directory
    mkdir -p "build/$component"
    
    # Determine build command
    local build_cmd
    if [ "$target" = "native" ]; then
        build_cmd="cargo build --release -p $component"
    elif [[ "$target" == linux* ]] || [ "$target" = "windows-x86_64" ]; then
        build_cmd="cross build --release --target $rust_target -p $component"
    else
        build_cmd="cargo build --release --target $rust_target -p $component"
    fi
    
    # Execute build command
    if ! $build_cmd; then
        print_color "RED" "Failed to build $component"
        exit 1
    fi
    
    # Copy binary to build directory
    if [[ $target == windows* ]]; then
        if [ "$target" = "native" ]; then
            cp "target/release/$component.exe" "build/$component/"
        else
            cp "target/$rust_target/release/$component.exe" "build/$component/"
        fi
    else
        if [ "$target" = "native" ]; then
            cp "target/release/$component" "build/$component/"
            chmod +x "build/$component/$component"
        else
            cp "target/$rust_target/release/$component" "build/$component/"
            chmod +x "build/$component/$component"
        fi
    fi
}

# Clean old build and release files
rm -rf build release

# Build components based on selection
case $selected_component in
    0|1)  # Both or Client
        # Download and extract FFmpeg
        download_ffmpeg "$target"
        build_component "ffmpeg-cluster-client" "$target" "$rust_target"
        ;;
esac

case $selected_component in
    0|2)  # Both or Server
        build_component "ffmpeg_cluster_server" "$target" "$rust_target"
        ;;
esac

# Create the release package
mkdir -p release
if [[ $target == windows* ]]; then
    print_color "BLUE" "Creating Windows ZIP archive..."
    cd build && zip -r "../release/ffmpeg-cluster-$target.zip" * && cd ..
else
    print_color "BLUE" "Creating tar.gz archive..."
    cd build && tar czf "../release/ffmpeg-cluster-$target.tar.gz" * && cd ..
fi

print_color "GREEN" "Build complete!"
print_color "BLUE" "Release package created in: release/ffmpeg-cluster-$target.$(if [[ $target == windows* ]]; then echo "zip"; else echo "tar.gz"; fi)"

# List the contents of the archive
echo
print_color "BLUE" "Archive contents:"
if [[ $target == windows* ]]; then
    unzip -l "release/ffmpeg-cluster-$target.zip"
else
    tar tvf "release/ffmpeg-cluster-$target.tar.gz"
fi