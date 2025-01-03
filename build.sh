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

show_usage() {
    echo "Usage: $0 [--test]"
    echo "  --test    Test mode - skips git operations and GitHub release"
}

clean_release_dir() {
    print_color "BLUE" "Cleaning release directory..."
    rm -rf "$RELEASE_DIR"/*
    mkdir -p "$RELEASE_DIR"
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
    mkdir -p "$BUILD_DIR/server"
    mkdir -p "$RELEASE_DIR"
}
# Function to download and extract FFmpeg
download_ffmpeg() {
    local os=$1
    # Convert to absolute paths
    local ffmpeg_cache_dir="$(pwd)/$CACHE_DIR/ffmpeg/$os"
    local temp_dir="$(pwd)/$TEMP_DIR/$os"

    mkdir -p "$ffmpeg_cache_dir"
    mkdir -p "$temp_dir"

    print_color "BLUE" "Setting up directories for FFmpeg ($os)..."

    # Check if FFmpeg binaries already exist in cache
    if [[ $os == windows* ]]; then
        if [ -f "$ffmpeg_cache_dir/ffmpeg.exe" ] && [ -f "$ffmpeg_cache_dir/ffprobe.exe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
            return
        fi
    else
        if [ -f "$ffmpeg_cache_dir/ffmpeg" ] && [ -f "$ffmpeg_cache_dir/ffprobe" ]; then
            print_color "GREEN" "Using cached FFmpeg for $os"
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
        rm -rf "$ffmpeg_dir" ffmpeg.tar.xz
        ;;
    "windows-x86_64")
        curl -L "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -o ffmpeg.zip
        unzip -o ffmpeg.zip
        cp "ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe" "$ffmpeg_cache_dir/"
        cp "ffmpeg-master-latest-win64-gpl/bin/ffprobe.exe" "$ffmpeg_cache_dir/"
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

    # Create unique temp directory for this target
    local target_temp_dir="$BUILD_DIR/temp/$target"
    mkdir -p "$target_temp_dir"

    # Build
    if [[ $target == macos* ]]; then
        # For macOS, always use native build
        print_color "BLUE" "Building natively for macOS..."
        if ! cargo build --release -p $package_name; then
            print_color "RED" "Failed to build $component"
            exit 1
        fi

        # Copy from native release directory
        cp "target/release/$package_name" "$target_temp_dir/" || {
            print_color "RED" "Failed to copy macOS executable"
            ls -la "target/release/"
            exit 1
        }
        chmod +x "$target_temp_dir/$package_name"
        print_color "GREEN" "Saved macOS executable to: $target_temp_dir/$package_name"

    elif [[ $target == windows* ]]; then
        # Windows needs cross
        print_color "BLUE" "Cross-compiling for Windows..."
        if ! cross build --release --target $rust_target -p $package_name; then
            print_color "RED" "Failed to build $component"
            exit 1
        fi

        cp "target/$rust_target/release/$package_name.exe" "$target_temp_dir/" || {
            print_color "RED" "Failed to copy Windows executable"
            ls -la "target/$rust_target/release/"
            exit 1
        }
        print_color "GREEN" "Saved Windows executable to: $target_temp_dir/$package_name.exe"

    elif [[ $target == linux* ]]; then
        # Linux needs cross
        print_color "BLUE" "Cross-compiling for Linux..."
        if ! cross build --release --target $rust_target -p $package_name; then
            print_color "RED" "Failed to build $component"
            exit 1
        fi

        cp "target/$rust_target/release/$package_name" "$target_temp_dir/" || {
            print_color "RED" "Failed to copy Linux executable"
            ls -la "target/$rust_target/release/"
            exit 1
        }
        chmod +x "$target_temp_dir/$package_name"
        print_color "GREEN" "Saved Linux executable to: $target_temp_dir/$package_name"
    fi

    print_color "BLUE" "Build artifacts in temp directory:"
    ls -la "$target_temp_dir"
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

    # Copy test video with better error handling and logging
    print_color "BLUE" "Looking for test video in: $base_dir/test.mp4"
    if [ -f "$base_dir/test.mp4" ]; then
        print_color "BLUE" "Found test.mp4, copying to samples directory..."
        cp "$base_dir/test.mp4" "$temp_release_dir/samples/" || {
            print_color "RED" "Failed to copy test.mp4"
            return 1
        }
        print_color "GREEN" "Successfully copied test.mp4 to samples directory"
    else
        print_color "YELLOW" "Warning: test.mp4 not found in: $base_dir"
        ls -la "$base_dir" | grep "test.mp4" || true
    fi

    # Helper function to copy FFmpeg binaries
    copy_ffmpeg_binaries() {
        local dest_dir="$1"
        local ffmpeg_cache_dir="$base_dir/$CACHE_DIR/ffmpeg/$target"

        if [[ $target == windows* ]]; then
            cp "$ffmpeg_cache_dir/ffmpeg.exe" "$dest_dir/"
            cp "$ffmpeg_cache_dir/ffprobe.exe" "$dest_dir/"
        else
            cp "$ffmpeg_cache_dir/ffmpeg" "$dest_dir/"
            cp "$ffmpeg_cache_dir/ffprobe" "$dest_dir/"
            chmod +x "$dest_dir/ffmpeg"
            chmod +x "$dest_dir/ffprobe"
        fi
    }

    # Function to find the correct executable path
    get_executable_path() {
        local component=$1
        local exe_name="ffmpeg-cluster-$component"
        local possible_paths=()

        # Always check temp directory first
        local target_temp_dir="$base_dir/$BUILD_DIR/temp/$target"

        if [[ $target == windows* ]]; then
            exe_name="${exe_name}.exe"
            possible_paths=(
                "$target_temp_dir/$exe_name"
                "$base_dir/target/x86_64-pc-windows-gnu/release/$exe_name"
                "$base_dir/target/release/$exe_name"
            )
        elif [[ $target == linux* ]]; then
            possible_paths=(
                "$target_temp_dir/$exe_name"
                "$base_dir/target/x86_64-unknown-linux-gnu/release/$exe_name"
                "$base_dir/target/release/$exe_name"
            )
        else
            possible_paths=(
                "$target_temp_dir/$exe_name"
                "$base_dir/target/release/$exe_name"
            )
        fi

        for path in "${possible_paths[@]}"; do
            if [ -f "$path" ]; then
                echo "$path"
                return 0
            fi
        done

        print_color "RED" "Executable not found in any of these locations:"
        for path in "${possible_paths[@]}"; do
            echo " - $path"
        done
        return 1
    }

    case $selected_component in
    0) # Both client and server
        if [[ $target == windows* ]]; then
            print_color "BLUE" "Copying Windows executables to release..."

            # Get client executable path
            local client_exe=$(get_executable_path "client")
            local server_exe=$(get_executable_path "server")

            if [ -z "$client_exe" ] || [ -z "$server_exe" ]; then
                exit 1
            fi

            if ! cp "$client_exe" "$temp_release_dir/ffmpeg-cluster-client.exe"; then
                print_color "RED" "Failed to copy Windows client executable"
                exit 1
            fi
            if ! cp "$server_exe" "$temp_release_dir/ffmpeg-cluster-server.exe"; then
                print_color "RED" "Failed to copy Windows server executable"
                exit 1
            fi
            copy_ffmpeg_binaries "$temp_release_dir/bin"

        elif [[ $target == linux* ]]; then
            print_color "BLUE" "Copying Linux executables to release..."

            # Get client executable path
            local client_exe=$(get_executable_path "client")
            local server_exe=$(get_executable_path "server")

            if [ -z "$client_exe" ] || [ -z "$server_exe" ]; then
                exit 1
            fi

            if ! cp "$client_exe" "$temp_release_dir/ffmpeg-cluster-client"; then
                print_color "RED" "Failed to copy Linux client executable"
                exit 1
            fi
            if ! cp "$server_exe" "$temp_release_dir/ffmpeg-cluster-server"; then
                print_color "RED" "Failed to copy Linux server executable"
                exit 1
            fi
            chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            chmod +x "$temp_release_dir/ffmpeg-cluster-server"
            copy_ffmpeg_binaries "$temp_release_dir/bin"

        else
            cp "$base_dir/$BUILD_DIR/client/ffmpeg-cluster-client" "$temp_release_dir/"
            cp "$base_dir/$BUILD_DIR/server/ffmpeg-cluster-server" "$temp_release_dir/"
            chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            chmod +x "$temp_release_dir/ffmpeg-cluster-server"
            copy_ffmpeg_binaries "$temp_release_dir/bin"
        fi
        ;;

    1) # Client only
        if [[ $target == macos* ]]; then
            print_color "BLUE" "Copying macOS client executable to release..."

            # First check temp directory
            local exe_source="$base_dir/$BUILD_DIR/temp/$target/ffmpeg-cluster-client"
            if [ ! -f "$exe_source" ]; then
                # Fallback to direct release directory
                exe_source="$base_dir/target/release/ffmpeg-cluster-client"
            fi

            print_color "BLUE" "Looking for macOS executable at: $exe_source"
            if [ ! -f "$exe_source" ]; then
                print_color "RED" "macOS executable not found at expected location"
                ls -la "$base_dir/target/release/"
                exit 1
            fi

            if ! cp "$exe_source" "$temp_release_dir/ffmpeg-cluster-client"; then
                print_color "RED" "Failed to copy macOS executable"
                exit 1
            fi
            chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            copy_ffmpeg_binaries "$temp_release_dir/bin"
        fi
        if [[ $target == windows* ]]; then
            print_color "BLUE" "Copying Windows client executable to release..."

            local exe_source=$(get_executable_path "client")
            if [ -z "$exe_source" ]; then
                exit 1
            fi

            if ! cp "$exe_source" "$temp_release_dir/ffmpeg-cluster-client.exe"; then
                print_color "RED" "Failed to copy Windows client executable"
                exit 1
            fi
            copy_ffmpeg_binaries "$temp_release_dir/bin"

        elif [[ $target == linux* ]]; then
            print_color "BLUE" "Copying Linux client executable to release..."

            local exe_source=$(get_executable_path "client")
            if [ -z "$exe_source" ]; then
                exit 1
            fi

            if ! cp "$exe_source" "$temp_release_dir/ffmpeg-cluster-client"; then
                print_color "RED" "Failed to copy Linux client executable"
                exit 1
            fi
            chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            copy_ffmpeg_binaries "$temp_release_dir/bin"

        else
            cp "$base_dir/$BUILD_DIR/client/ffmpeg-cluster-client" "$temp_release_dir/"
            chmod +x "$temp_release_dir/ffmpeg-cluster-client"
            copy_ffmpeg_binaries "$temp_release_dir/bin"
        fi
        ;;

    2) # Server only
        if [[ $target == windows* ]]; then
            print_color "BLUE" "Copying Windows server executable to release..."

            local exe_source=$(get_executable_path "server")
            if [ -z "$exe_source" ]; then
                exit 1
            fi

            if ! cp "$exe_source" "$temp_release_dir/ffmpeg-cluster-server.exe"; then
                print_color "RED" "Failed to copy Windows server executable"
                exit 1
            fi

        elif [[ $target == linux* ]]; then
            print_color "BLUE" "Copying Linux server executable to release..."

            local exe_source=$(get_executable_path "server")
            if [ -z "$exe_source" ]; then
                exit 1
            fi

            if ! cp "$exe_source" "$temp_release_dir/ffmpeg-cluster-server"; then
                print_color "RED" "Failed to copy Linux server executable"
                exit 1
            fi
            chmod +x "$temp_release_dir/ffmpeg-cluster-server"
        else
            cp "$base_dir/$BUILD_DIR/server/ffmpeg-cluster-server" "$temp_release_dir/"
            chmod +x "$temp_release_dir/ffmpeg-cluster-server"
        fi
        ;;
    esac

    mkdir -p "$base_dir/$RELEASE_DIR"

    if [[ $target == windows* ]]; then
        print_color "BLUE" "Creating Windows ZIP archive..."
        (cd "$temp_release_dir" && zip -r "$base_dir/$RELEASE_DIR/ffmpeg-cluster-$target.zip" ./*) || {
            print_color "RED" "Failed to create ZIP archive"
            return 1
        }
    elif [[ $target == macos* ]]; then
        print_color "BLUE" "Creating macOS archive..."
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
    if [ $TEST_MODE -eq 0 ]; then
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

        # Update version in Cargo.toml files only in non-test mode
        if [[ "$(uname)" == "Darwin" ]]; then
            # Update all Cargo.toml files including the root one
            sed -i '' "s/^version = \".*\"/version = \"$version\"/" Cargo.toml */Cargo.toml
            [ -f .env ] && sed -i '' "s/^VERSION=.*/VERSION=$version/" .env
        else
            sed -i "s/^version = \".*\"/version = \"$version\"/" Cargo.toml */Cargo.toml
            [ -f .env ] && sed -i "s/^VERSION=.*/VERSION=$version/" .env
        fi

        set_environment
    else
        commit_message="Test release"
        version="0.0.0-test"
        print_color "YELLOW" "Test mode: Using default values (no Cargo.toml updates)"
    fi
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
# Main function
main() {
    # Handle parameters
    TEST_MODE=0
    while [[ "$#" -gt 0 ]]; do
        case $1 in
        --test)
            TEST_MODE=1
            print_color "YELLOW" "Test mode: Will skip all git operations and version updates"
            commit_message="Test release"
            version="0.0.0-test"
            ;;
        --help | -h)
            show_usage
            exit 0
            ;;
        *)
            print_color "RED" "Unknown parameter: $1"
            show_usage
            exit 1
            ;;
        esac
        shift
    done

    start_task "Process"

    set_environment
    check_requirements
    setup_build_dirs
    clean_release_dir

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
        if [ $TEST_MODE -eq 0 ]; then
            input
        fi

        # Track successful builds
        local successful_builds=()

        # Build for macOS x86_64
        NATIVE_TARGET="macos-x86_64"
        NATIVE_RUST_TARGET="x86_64-apple-darwin"
        print_color "BLUE" "Building for macOS (x86_64)..."
        if download_ffmpeg "$NATIVE_TARGET" &&
            build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET" &&
            create_release_package "$NATIVE_TARGET" "1"; then
            successful_builds+=("macOS x86_64")
            print_color "GREEN" "Successfully built for macOS x86_64"
        else
            print_color "YELLOW" "Skipping macOS x86_64 due to build failure"
        fi

        # Build for Windows x86_64
        NATIVE_TARGET="windows-x86_64"
        NATIVE_RUST_TARGET="x86_64-pc-windows-gnu"
        print_color "BLUE" "Building for Windows (x86_64)..."
        if download_ffmpeg "$NATIVE_TARGET" &&
            build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET" &&
            create_release_package "$NATIVE_TARGET" "1"; then
            successful_builds+=("Windows x86_64")
            print_color "GREEN" "Successfully built for Windows x86_64"
        else
            print_color "YELLOW" "Skipping Windows x86_64 due to build failure"
        fi

        # Build for Linux x86_64
        NATIVE_TARGET="linux-x86_64"
        NATIVE_RUST_TARGET="x86_64-unknown-linux-gnu"
        print_color "BLUE" "Building for Linux (x86_64)..."
        if download_ffmpeg "$NATIVE_TARGET" &&
            build_component "client" "$NATIVE_TARGET" "$NATIVE_RUST_TARGET" &&
            create_release_package "$NATIVE_TARGET" "1"; then
            successful_builds+=("Linux x86_64")
            print_color "GREEN" "Successfully built for Linux x86_64"
        else
            print_color "YELLOW" "Skipping Linux x86_64 due to build failure"
        fi

        if [ ${#successful_builds[@]} -eq 0 ]; then
            print_color "RED" "No platforms were built successfully"
            exit 1
        else
            print_color "GREEN" "Successfully built for: ${successful_builds[*]}"
            if [ $TEST_MODE -eq 0 ]; then
                tag_and_push
                create_github_release
            fi
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

        if [ $TEST_MODE -eq 0 ]; then
            input
        fi

        # Download FFmpeg and build with error handling
        if ! download_ffmpeg "$target"; then
            print_color "RED" "Failed to download FFmpeg for $target"
            exit 1
        fi

        case $selected_component in
        "1")
            if ! build_component "client" "$target" "$rust_target" ||
                ! create_release_package "$target" "1"; then
                print_color "RED" "Failed to build client for $target"
                exit 1
            fi
            ;;
        "2")
            if ! build_component "server" "$target" "$rust_target" ||
                ! create_release_package "$target" "2"; then
                print_color "RED" "Failed to build server for $target"
                exit 1
            fi
            ;;
        "0")
            if ! build_component "client" "$target" "$rust_target" ||
                ! build_component "server" "$target" "$rust_target" ||
                ! create_release_package "$target" "0"; then
                print_color "RED" "Failed to build components for $target"
                exit 1
            fi
            ;;
        esac

        if [ $TEST_MODE -eq 0 ] && [ $? -eq 0 ]; then
            tag_and_push
            create_github_release
        fi
        ;;
    esac

    end_task
}
main "$@"
