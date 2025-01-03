#!/bin/bash

# Colors for better UI
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Timing and task variables
START=0
END=0
NAME=""

# Function to set environment variables from .env file
set_environment() {
    if [ -f .env ]; then
        set -o allexport
        source .env
        set +o allexport
    else
        print_color "YELLOW" "No .env file found, using default values"
    fi
}

# Function to print in color
print_color() {
    printf "${!1}%s${NC}\n" "$2"
}

# Function to print menu header
print_menu_header() {
    printf "\n─── %s ───\n\n" "$1"
}

# Function to center text
center_text() {
    local text="$1"
    local width=32
    local padding=$(((width - ${#text}) / 2))
    printf "%*s%s%*s" $padding "" "$text" $((width - padding - ${#text})) ""
}

# Function to print menu option
print_menu_option() {
    printf "${GREEN}  %s${NC}) ${YELLOW}%s${NC}\n" "$1" "$2"
}

# Function to start timing a task
start_task() {
    START=$(date +%s)
    NAME="$1"
    print_color "BLUE" "Starting task: $NAME"
}

# Function to end timing a task
end_task() {
    END=$(date +%s)
    local minutes=$((($END - $START) / 60))
    local seconds=$((($END - $START) % 60))
    if [[ $minutes != 0 ]]; then
        minutes="$minutes min. and "
    else
        minutes=""
    fi
    echo ""
    print_color "GREEN" "⏱️  $NAME completed in: ${minutes}${seconds} s"
    echo ""
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
    MINGW* | MSYS* | CYGWIN*)
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

# Check for required tools
check_requirements() {
    MISSING_TOOLS=()
    for tool in "cargo" "curl" "unzip" "tar" "gh"; do
        if ! command_exists "$tool"; then
            MISSING_TOOLS+=("$tool")
        fi
    done

    if [ ${#MISSING_TOOLS[@]} -ne 0 ]; then
        print_color "RED" "Missing required tools: ${MISSING_TOOLS[*]}"
        print_color "YELLOW" "Please install them and try again."
        exit 1
    fi
}

# Function to handle build errors
handle_build_error() {
    print_color "RED" "Build failed! See error message above."
    rm -rf build release
    exit 1
}

# Set error trap
trap 'handle_build_error' ERR

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
# Function to download and extract FFmpeg
download_ffmpeg() {
    local os=$1
    # Convert to absolute paths
    local ffmpeg_cache_dir="$(pwd)/$CACHE_DIR/ffmpeg/$os"
    local temp_dir="$(pwd)/$TEMP_DIR/$os"
    local client_bin_dir="$(pwd)/$BUILD_DIR/client/bin"

    mkdir -p "$ffmpeg_cache_dir"
    mkdir -p "$temp_dir"
    mkdir -p "$client_bin_dir"

    print_color "BLUE" "Setting up directories for FFmpeg ($os)..."

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
    "macos-universal2" | "macos-x86_64" | "macos-aarch64")
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
    "linux-x86_64" | "linux-aarch64")
        local arch_suffix="amd64"
        if [[ "$os" == *"aarch64"* ]]; then
            arch_suffix="arm64"
        fi

        curl -L "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-${arch_suffix}-static.tar.xz" -o ffmpeg.tar.xz
        tar xf ffmpeg.tar.xz
        ffmpeg_dir=$(find . -maxdepth 1 -type d -name "ffmpeg-*-${arch_suffix}-static" | head -n 1)

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

    # Map component name to package name
    local package_name
    case $component in
    "client")
        package_name="ffmpeg-cluster-client"
        ;;
    "server")
        package_name="ffmpeg-cluster-server"
        ;;
    *)
        print_color "RED" "Unknown component: $component"
        exit 1
        ;;
    esac

    mkdir -p "$BUILD_DIR/$component"

    local build_cmd
    if [[ "$target" == "$NATIVE_TARGET" ]]; then
        build_cmd="cargo build --release -p $package_name"
    elif [[ "$target" == linux* ]] || [ "$target" = "windows-x86_64" ]; then
        build_cmd="cross build --release --target $rust_target -p $package_name"
    else
        build_cmd="cargo build --release --target $rust_target -p $package_name"
    fi

    if ! $build_cmd; then
        print_color "RED" "Failed to build $component"
        exit 1
    fi

    if [[ $target == windows* ]]; then
        if [[ "$target" == "$NATIVE_TARGET" ]]; then
            cp "target/release/$package_name.exe" "$BUILD_DIR/$component/"
        else
            cp "target/$rust_target/release/$package_name.exe" "$BUILD_DIR/$component/"
        fi
    else
        if [[ "$target" == "$NATIVE_TARGET" ]]; then
            cp "target/release/$package_name" "$BUILD_DIR/$component/"
            chmod +x "$BUILD_DIR/$component/$package_name"
        else
            cp "target/$rust_target/release/$package_name" "$BUILD_DIR/$component/"
            chmod +x "$BUILD_DIR/$component/$package_name"
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
    mkdir -p "$temp_release_dir/samples"

    # Copy test video if it exists
    if [ -f "test.mp4" ]; then
        print_color "BLUE" "Including test video sample..."
        cp "test.mp4" "$temp_release_dir/samples/"
    else
        print_color "YELLOW" "Warning: test.mp4 not found in project root"
    fi

    # Rest of the existing function code...
    # (keep the existing copy_ffmpeg_binaries and component selection logic)

    mkdir -p "$base_dir/$RELEASE_DIR"

    if [[ $target == windows* ]]; then
        print_color "BLUE" "Creating Windows ZIP archive..."
        (cd "$temp_release_dir" && zip -r "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip" ./*) || {
            print_color "RED" "Failed to create ZIP archive"
            return 1
        }
    elif [[ $target == macos* ]]; then
        print_color "BLUE" "Creating macOS archive..."
        # Using ditto for better macOS compatibility
        if command -v ditto >/dev/null 2>&1; then
            (cd "$temp_release_dir" && ditto -c -k --sequesterRsrc --keepParent . "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip") || {
                print_color "RED" "Failed to create ZIP archive with ditto"
                print_color "YELLOW" "Falling back to tar.gz..."
                (cd "$temp_release_dir" && tar czf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz" ./*) || {
                    print_color "RED" "Failed to create tar.gz archive"
                    return 1
                }
            }
        else
            print_color "YELLOW" "ditto not available, using tar.gz for macOS..."
            (cd "$temp_release_dir" && tar czf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz" ./*) || {
                print_color "RED" "Failed to create tar.gz archive"
                return 1
            }
        fi
    else
        print_color "BLUE" "Creating tar.gz archive..."
        (cd "$temp_release_dir" && tar czf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz" ./*) || {
            print_color "RED" "Failed to create tar.gz archive"
            return 1
        }
    fi

    rm -rf "$temp_release_dir"

    echo
    print_color "BLUE" "Archive contents:"
    if [[ $target == windows* ]] || ([[ $target == macos* ]] && [ -f "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip" ]); then
        unzip -l "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip"
    else
        tar tvf "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.tar.gz"
    fi
}

# Function to prompt for input
input() {
    echo ""
    valid_commit=0
    while [[ $valid_commit -eq 0 ]]; do
        print_color "BLUE" "> Enter commit message"
        echo ""
        read commit_message
        if [[ -z "$commit_message" ]]; then
            print_color "RED" "Commit message cannot be empty."
        else
            valid_commit=1
        fi
    done

    echo ""
    valid_version=0
    while [[ $valid_version -eq 0 ]]; do
        print_color "BLUE" "> Enter version"
        echo ""
        read version
        if [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            valid_version=1
        else
            print_color "RED" "Version must be in semantic versioning format (X.Y.Z). Please try again."
        fi
    done

    # Update version in Cargo.toml files
    if [[ "$(uname)" == "Darwin" ]]; then
        # Update all Cargo.toml files including the root one
        sed -i '' "s/^version = \".*\"/version = \"$version\"/" Cargo.toml */Cargo.toml
        [ -f .env ] && sed -i '' "s/^VERSION=.*/VERSION=$version/" .env
    else
        sed -i "s/^version = \".*\"/version = \"$version\"/" Cargo.toml */Cargo.toml
        [ -f .env ] && sed -i "s/^VERSION=.*/VERSION=$version/" .env
    fi

    set_environment
}

# Function to tag and push to git
tag_and_push() {
    print_color "BLUE" "Creating git tag v$version..."
    git tag -a "v$version" -m "$commit_message"
    git push origin "v$version"

    print_color "BLUE" "Committing changes..."
    git add .
    git commit -m "$commit_message"
    git push
}

# Function to create GitHub release
create_github_release() {
    print_color "BLUE" "🚀 Creating GitHub release..."

    RELEASE_NOTES=$(mktemp)
    git log $(git describe --tags --abbrev=0)..HEAD --pretty=format:"- %s (%h)" >>"$RELEASE_NOTES"

    gh release create "v${version}" \
        --title "v${version}" \
        --notes-file "$RELEASE_NOTES" \
        $RELEASE_DIR/*

    rm "$RELEASE_NOTES"
}

# Main function
main() {
    start_task "Process"

    set_environment
    check_requirements
    setup_build_dirs

    # Initial action selection
    print_menu_header "Action Selection"
    print_menu_option "1" "Build for single platform"
    print_menu_option "2" "Create release (macOS + Windows + Linux)"
    print_menu_option "3" "Quit"
    printf "\nSelect action [1]: "
    read -r action_choice

    case $action_choice in
    "2") # Release workflow
        print_color "BLUE" "Starting release process..."
        input # Get version and commit message

        # Build for macOS x86_64
        NATIVE_TARGET="macos-x86_64"
        NATIVE_RUST_TARGET="x86_64-apple-darwin"
        print_color "BLUE" "Building for macOS (x86_64)..."
        download_ffmpeg "$NATIVE_TARGET"
        build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET"
        create_release_package "$NATIVE_TARGET" "1"

        # Build for Windows x86_64
        NATIVE_TARGET="windows-x86_64"
        NATIVE_RUST_TARGET="x86_64-pc-windows-gnu"
        print_color "BLUE" "Building for Windows (x86_64)..."
        download_ffmpeg "$NATIVE_TARGET"
        build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET"
        create_release_package "$NATIVE_TARGET" "1"

        # Build for Linux x86_64
        NATIVE_TARGET="linux-x86_64"
        NATIVE_RUST_TARGET="x86_64-unknown-linux-gnu"
        print_color "BLUE" "Building for Linux (x86_64)..."
        download_ffmpeg "$NATIVE_TARGET"
        build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET"
        create_release_package "$NATIVE_TARGET" "1"

        # Create GitHub release
        if [ $? -eq 0 ]; then
            tag_and_push
            create_github_release
        fi
        ;;

    "3") # Quit
        print_color "YELLOW" "Exiting..."
        exit 0
        ;;

    *) # Single platform build (default)
        # Get native platform information
        NATIVE_PLATFORM_INFO=$(detect_native_platform)
        NATIVE_TARGET=$(echo $NATIVE_PLATFORM_INFO | cut -d'|' -f1)
        NATIVE_RUST_TARGET=$(echo $NATIVE_PLATFORM_INFO | cut -d'|' -f2)

        # Platform selection menu
        print_menu_header "Platform Selection"
        print_menu_option "1" "macOS (Universal)"
        print_menu_option "2" "macOS (x86_64)"
        print_menu_option "3" "macOS (ARM64)"
        print_menu_option "4" "Linux (x86_64)"
        print_menu_option "5" "Linux (ARM64)"
        print_menu_option "6" "Windows (x86_64)"
        print_menu_option "7" "Native [default]"
        printf "\nSelect platform [7]: "
        read -r platform_choice

        # Map platform choice
        case $platform_choice in
        "1")
            target="macos-universal2"
            rust_target="universal2-apple-darwin"
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
            check_docker
            check_cross
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
        *)
            target="$NATIVE_TARGET"
            rust_target="$NATIVE_RUST_TARGET"
            ;;
        esac

        # Component selection menu
        print_menu_header "Component Selection"
        print_menu_option "1" "Both (Client + Server) [default]"
        print_menu_option "2" "Client only"
        print_menu_option "3" "Server only"
        printf "\nSelect components to build [1]: "
        read -r component_choice

        case $component_choice in
        "2")
            selected_component="1" # Client only
            ;;
        "3")
            selected_component="2" # Server only
            ;;
        *)
            selected_component="0" # Both (default)
            ;;
        esac

        # Get version and commit message
        input

        # Download FFmpeg and build
        download_ffmpeg "$target"
        case $selected_component in
        "1")
            build_component "client" "$target" "$rust_target"
            ;;
        "2")
            build_component "server" "$target" "$rust_target"
            ;;
        "0")
            build_component "client" "$target" "$rust_target"
            build_component "server" "$target" "$rust_target"
            ;;
        esac

        create_release_package "$target" "$selected_component"

        if [ $? -eq 0 ]; then
            tag_and_push
            create_github_release
        fi
        ;;
    esac

    end_task
}

main "$@"
