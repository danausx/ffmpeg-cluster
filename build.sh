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

# Function to detect native platform
detect_native_platform() {
    local os=$(uname -s)
    local arch=$(uname -m)
    local native_target=""
    local native_rust_target=""

    case "$os" in
        Darwin)
            case "$arch" in
                x86_64)
                    native_target="macos-x86_64"
                    native_rust_target="x86_64-apple-darwin"
                    ;;
                arm64)
                    native_target="macos-aarch64"
                    native_rust_target="aarch64-apple-darwin"
                    ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64)
                    native_target="linux-x86_64"
                    native_rust_target="x86_64-unknown-linux-gnu"
                    ;;
                aarch64)
                    native_target="linux-aarch64"
                    native_rust_target="aarch64-unknown-linux-gnu"
                    ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            native_target="windows-x86_64"
            native_rust_target="x86_64-pc-windows-gnu"
            ;;
    esac

    echo "$native_target|$native_rust_target"
}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check for required tools silently
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
    if ! command_exists cross; then
        print_color "YELLOW" "Cross not found. Installing cross..."
        cargo install cross || {
            print_color "RED" "Failed to install cross"
            exit 1
        }
        print_color "GREEN" "Cross installed successfully"
    else
        print_color "GREEN" "Cross is already installed"
    fi

    # Pull the correct Docker image based on target
    print_color "BLUE" "Pulling Docker image for $rust_target..."
    
    # Only use platform flag for x86_64 targets when on ARM64
    if [[ "$rust_target" == *"x86_64"* ]]; then
        docker pull --platform linux/amd64 "ghcr.io/cross-rs/$rust_target:main" || {
            print_color "RED" "Failed to pull Docker image for $rust_target"
            exit 1
        }
    else
        docker pull "ghcr.io/cross-rs/$rust_target:main" || {
            print_color "RED" "Failed to pull Docker image for $rust_target"
            exit 1
        }
    fi
}

# Function to check Docker
check_docker() {
    if ! command_exists docker; then
        print_color "RED" "Docker is required for cross compilation. Please install Docker"
        exit 1
    fi
    
    print_color "BLUE" "Testing Docker connection..."
    if ! docker info >/dev/null 2>&1; then
        print_color "RED" "Docker is not running. Please start Docker"
        exit 1
    fi
    print_color "GREEN" "Docker is running and accessible"
}

# Function to print menu header
print_menu_header() {
    printf "\n─── %s ───\n\n" "$1"
}

# Function to center text
center_text() {
    local text="$1"
    local width=32
    local padding=$(( (width - ${#text}) / 2 ))
    printf "%*s%s%*s" $padding "" "$text" $(( width - padding - ${#text} )) ""
}

# Function to print menu option
print_menu_option() {
    printf "${GREEN}  %s${NC}) ${YELLOW}%s${NC}\n" "$1" "$2"
}

# Get native platform information
NATIVE_PLATFORM_INFO=$(detect_native_platform)
NATIVE_TARGET=$(echo $NATIVE_PLATFORM_INFO | cut -d'|' -f1)
NATIVE_RUST_TARGET=$(echo $NATIVE_PLATFORM_INFO | cut -d'|' -f2)

# Component selection menu
print_menu_header "Component Selection"
print_menu_option "1" "Both (Client + Server) [default]"
print_menu_option "2" "Client only"
print_menu_option "3" "Server only"
printf "\nSelect components to build [1]: "
read -r component_choice

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

# Platform selection menu
print_menu_header "Platform Selection"
print_menu_option "1" "macOS (Universal)"
print_menu_option "2" "macOS (x86_64)"
print_menu_option "3" "macOS (ARM64)"
print_menu_option "4" "Linux (x86_64)"
print_menu_option "5" "Linux (ARM64)"
print_menu_option "6" "Windows (x86_64)"
print_menu_option "7" "$NATIVE_TARGET [default]"
print_menu_option "8" "Quit"
printf "\nEnter your choice (1-8) [7]: "
read -r platform_choice

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
        rust_target="x86_64-unknown-linux-gnu"
        print_color "BLUE" "Checking Docker..."
        check_docker
        print_color "BLUE" "Checking Cross..."
        check_cross
        print_color "BLUE" "Setup complete for Linux x86_64"
        ;;
    "5")
        target="linux-aarch64"
        rust_target="aarch64-unknown-linux-gnu"
        check_docker
        check_cross
        ;;
    "6")
        target="windows-x86_64"
        rust_target="x86_64-pc-windows-gnu"
        check_cross
        ;;
    "8")
        print_color "YELLOW" "Build cancelled."
        exit 0
        ;;
    *)
    target="$NATIVE_TARGET"
    rust_target="$NATIVE_RUST_TARGET"
    if [[ "$target" == linux* ]]; then
        check_docker
        check_cross
    fi
    ;;
esac

# Main directories
BUILD_DIR="build"
CACHE_DIR="$BUILD_DIR/cache"
TEMP_DIR="$BUILD_DIR/temp"
RELEASE_DIR="release"

# Function to setup build directories
setup_build_dirs() {
    mkdir -p "$CACHE_DIR/ffmpeg"
    mkdir -p "$TEMP_DIR"
    mkdir -p "$BUILD_DIR/client/bin"
    mkdir -p "$BUILD_DIR/server"
    mkdir -p "$RELEASE_DIR"
}

# Setup build directories
setup_build_dirs

# Function to download and extract FFmpeg
download_ffmpeg() {
    local os=$1
    # Convert to absolute paths
    local ffmpeg_cache_dir="$(pwd)/$CACHE_DIR/ffmpeg/$os"
    local temp_dir="$(pwd)/$TEMP_DIR/$os"
    local client_bin_dir="$(pwd)/$BUILD_DIR/client/bin"
    
    # Create all necessary directories first
    mkdir -p "$ffmpeg_cache_dir"
    mkdir -p "$temp_dir"
    mkdir -p "$client_bin_dir"
    
    print_color "BLUE" "Setting up directories for FFmpeg ($os)..."
    print_color "BLUE" "Cache dir: $ffmpeg_cache_dir"
    print_color "BLUE" "Temp dir: $temp_dir"
    print_color "BLUE" "Client bin dir: $client_bin_dir"

    # Check if FFmpeg binaries already exist in cache
    if [[ $os == windows* ]]; then
        if [ -f "$ffmpeg_cache_dir/ffmpeg.exe" ] && [ -f "$ffmpeg_cache_dir/ffprobe.exe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
            cp "$ffmpeg_cache_dir/ffmpeg.exe" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe.exe" "$client_bin_dir/"
            return
        fi
    else
        if [ -f "$ffmpeg_cache_dir/ffmpeg" ] && [ -f "$ffmpeg_cache_dir/ffprobe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
            cp "$ffmpeg_cache_dir/ffmpeg" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe" "$client_bin_dir/"
            chmod +x "$client_bin_dir/ffmpeg"
            chmod +x "$client_bin_dir/ffprobe"
            return
        fi
    fi

    print_color "BLUE" "Downloading FFmpeg for $os..."
    
    cd "$temp_dir" || exit 1

    case "$os" in
        "macos-universal2"|"macos-x86_64"|"macos-aarch64")
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip" -o ffmpeg.zip
            curl -L "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip" -o ffprobe.zip
            
            unzip -o ffmpeg.zip
            unzip -o ffprobe.zip
            cp ffmpeg "$ffmpeg_cache_dir/"
            cp ffprobe "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe" "$client_bin_dir/"
            chmod +x "$client_bin_dir/ffmpeg"
            chmod +x "$client_bin_dir/ffprobe"
            rm -f ffmpeg.zip ffprobe.zip
            ;;

        "linux-x86_64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            ffmpeg_dir=$(find . -maxdepth 1 -type d -name "ffmpeg-*-amd64-static" | head -n 1)
            if [ -z "$ffmpeg_dir" ]; then
                print_color "RED" "Failed to find extracted FFmpeg directory"
                exit 1
            fi
            cp "$ffmpeg_dir/ffmpeg" "$ffmpeg_cache_dir/"
            cp "$ffmpeg_dir/ffprobe" "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe" "$client_bin_dir/"
            chmod +x "$client_bin_dir/ffmpeg"
            chmod +x "$client_bin_dir/ffprobe"
            rm -rf "$ffmpeg_dir" ffmpeg.tar.xz
            ;;

        "linux-aarch64")
            curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz" -o ffmpeg.tar.xz
            tar xf ffmpeg.tar.xz
            ffmpeg_dir=$(find . -maxdepth 1 -type d -name "ffmpeg-*-arm64-static" | head -n 1)
            if [ -z "$ffmpeg_dir" ]; then
                print_color "RED" "Failed to find extracted FFmpeg directory"
                exit 1
            fi
            cp "$temp_dir/$ffmpeg_dir/ffmpeg" "$ffmpeg_cache_dir/"
            cp "$temp_dir/$ffmpeg_dir/ffprobe" "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe" "$client_bin_dir/"
            chmod +x "$client_bin_dir/ffmpeg"
            chmod +x "$client_bin_dir/ffprobe"
            rm -rf "$ffmpeg_dir" ffmpeg.tar.xz
            ;;

        "windows-x86_64")
            curl -L "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -o ffmpeg.zip
            unzip -o ffmpeg.zip
            cp "ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe" "$ffmpeg_cache_dir/"
            cp "ffmpeg-master-latest-win64-gpl/bin/ffprobe.exe" "$ffmpeg_cache_dir/"
            cp "$ffmpeg_cache_dir/ffmpeg.exe" "$client_bin_dir/"
            cp "$ffmpeg_cache_dir/ffprobe.exe" "$client_bin_dir/"
            rm -rf ffmpeg-master-latest-win64-gpl ffmpeg.zip
            ;;
    esac

    cd - >/dev/null || exit 1
}

# Function to build component
build_component() {
    local component=$1
    local target=$2
    local rust_target=$3
    
    print_color "BLUE" "Building $component for $target..."
    
    # Create build directory
    mkdir -p "$BUILD_DIR/$component"
    
# Determine build command
    local build_cmd
    if [[ "$target" == "$NATIVE_TARGET" ]]; then
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
        if [[ "$target" == "$NATIVE_TARGET" ]]; then
            cp "target/release/$component.exe" "$BUILD_DIR/$component/"
        else
            cp "target/$rust_target/release/$component.exe" "$BUILD_DIR/$component/"
        fi
    else
        if [[ "$target" == "$NATIVE_TARGET" ]]; then
            cp "target/release/$component" "$BUILD_DIR/$component/"
            chmod +x "$BUILD_DIR/$component/$component"
        else
            cp "target/$rust_target/release/$component" "$BUILD_DIR/$component/"
            chmod +x "$BUILD_DIR/$component/$component"
        fi
    fi
}

# Function to create the release package
create_release_package() {
    local target=$1
    local selected_component=$2
    local base_dir=$(pwd)
    local temp_release_dir="$base_dir/$TEMP_DIR/release_package"
    
    print_color "BLUE" "Preparing release package..."
    rm -rf "$temp_release_dir"
    mkdir -p "$temp_release_dir/bin"
    
    # Helper function to copy FFmpeg binaries
    copy_ffmpeg_binaries() {
        local dest_dir="$1"
        if [[ $target == windows* ]]; then
            cp "$base_dir/$BUILD_DIR/client/bin/ffmpeg.exe" "$dest_dir/bin/"
            cp "$base_dir/$BUILD_DIR/client/bin/ffprobe.exe" "$dest_dir/bin/"
        else
            cp "$base_dir/$BUILD_DIR/client/bin/ffmpeg" "$dest_dir/bin/"
            cp "$base_dir/$BUILD_DIR/client/bin/ffprobe" "$dest_dir/bin/"
            chmod +x "$dest_dir/bin/ffmpeg"
            chmod +x "$dest_dir/bin/ffprobe"
        fi
    }
    
    # Copy files based on component selection
    case $selected_component in
        0)  # Both client and server
            # Copy client files
            if [[ $target == windows* ]]; then
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-client/ffmpeg-cluster-client.exe" "$temp_release_dir/"
            else
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-client/ffmpeg-cluster-client" "$temp_release_dir/"
                chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            fi
            
            # Copy server files
            if [[ $target == windows* ]]; then
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-server/ffmpeg-cluster-server.exe" "$temp_release_dir/"
            else
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-server/ffmpeg-cluster-server" "$temp_release_dir/"
                chmod +x "$temp_release_dir/ffmpeg-cluster-server"
            fi
            
            # Copy FFmpeg binaries
            copy_ffmpeg_binaries "$temp_release_dir"
            ;;
            
        1)  # Client only
            if [[ $target == windows* ]]; then
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-client/ffmpeg-cluster-client.exe" "$temp_release_dir/"
            else
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-client/ffmpeg-cluster-client" "$temp_release_dir/"
                chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            fi
            
            # Copy FFmpeg binaries
            copy_ffmpeg_binaries "$temp_release_dir"
            ;;
            
        2)  # Server only
            if [[ $target == windows* ]]; then
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-server/ffmpeg-cluster-server.exe" "$temp_release_dir/"
            else
                cp "$base_dir/$BUILD_DIR/ffmpeg-cluster-server/ffmpeg-cluster-server" "$temp_release_dir/"
                chmod +x "$temp_release_dir/ffmpeg-cluster-server"
            fi
            
            # Copy FFmpeg binaries for server
            copy_ffmpeg_binaries "$temp_release_dir"
            ;;
    esac
    
    # Create the release directory if it doesn't exist
    mkdir -p "$base_dir/$RELEASE_DIR"
    
    # Create the final archive
    if [[ $target == windows* ]]; then
        print_color "BLUE" "Creating Windows ZIP archive..."
        (cd "$temp_release_dir" && zip -r "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip" ./*) || {
            print_color "RED" "Failed to create ZIP archive"
            return 1
        }
    else
        print_color "BLUE" "Creating tar.gz archive..."
        (cd "$temp_release_dir" && tar czf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz" ./*) || {
            print_color "RED" "Failed to create tar.gz archive"
            return 1
        }
    fi
    
    # Clean up
    rm -rf "$temp_release_dir"
    
    # List the contents of the archive
    echo
    print_color "BLUE" "Archive contents:"
    if [[ $target == windows* ]]; then
        unzip -l "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip"
    else
        tar tvf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz"
    fi
}

# Clean only release files, keep build cache
rm -rf release
mkdir -p build

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
        # Download FFmpeg for server if it wasn't already downloaded
        if [ "$selected_component" = "2" ]; then
            download_ffmpeg "$target"
        fi
        build_component "ffmpeg-cluster-server" "$target" "$rust_target"
        ;;
esac

# Create and package the release files
create_release_package "$target" "$selected_component"

print_color "GREEN" "Build complete!"
print_color "BLUE" "Release package created in: release/ffmpeg-cluster-$target.$(if [[ $target == windows* ]]; then echo "zip"; else echo "tar.gz"; fi)"