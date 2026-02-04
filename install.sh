#!/usr/bin/env bash
# trx installer - Install trx issue tracker binaries to user space
#
# Quick install:
#   curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash
#
# With options:
#   curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash -s -- --version 0.2.1
#   curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash -s -- --prefix ~/bin
#
# Or download and run:
#   curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh > install.sh
#   chmod +x install.sh
#   ./install.sh [--version X.Y.Z] [--prefix DIR] [--uninstall]

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
REPO="cloonix/trx"
DEFAULT_PREFIX="$HOME/.local/bin"
INSTALL_PREFIX="${TRX_INSTALL_PREFIX:-$DEFAULT_PREFIX}"
VERSION="${TRX_VERSION:-latest}"
COMPONENTS=("trx" "trx-tui" "trx-api" "trx-mcp")

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --prefix)
            INSTALL_PREFIX="$2"
            shift 2
            ;;
        --uninstall)
            UNINSTALL=1
            shift
            ;;
        -h|--help)
            cat <<EOF
trx installer - Install trx issue tracker binaries

Usage: $0 [OPTIONS]

Options:
    --version X.Y.Z    Install specific version (default: latest)
    --prefix DIR       Install to directory (default: ~/.local/bin)
    --uninstall        Remove installed binaries
    -h, --help         Show this help message

Environment Variables:
    TRX_INSTALL_PREFIX  Override default installation prefix
    TRX_VERSION         Override default version

Examples:
    # Install latest version
    curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash

    # Install specific version
    curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash -s -- --version 0.2.1

    # Install to custom directory
    ./install.sh --prefix ~/bin

    # Uninstall
    ./install.sh --uninstall

EOF
            exit 0
            ;;
        *)
            echo -e "${RED}Error: Unknown option: $1${NC}" >&2
            echo "Run with --help for usage information" >&2
            exit 1
            ;;
    esac
done

# Helper functions
info() {
    echo -e "${BLUE}==>${NC} $1"
}

success() {
    echo -e "${GREEN}✓${NC} $1"
}

warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

error() {
    echo -e "${RED}✗ Error:${NC} $1" >&2
    exit 1
}

# Uninstall function
uninstall() {
    info "Uninstalling trx from $INSTALL_PREFIX"
    
    local removed=0
    for component in "${COMPONENTS[@]}"; do
        local bin_path="$INSTALL_PREFIX/$component"
        if [[ -f "$bin_path" ]]; then
            rm -f "$bin_path"
            success "Removed $component"
            removed=$((removed + 1))
        fi
    done
    
    if [[ $removed -eq 0 ]]; then
        warning "No trx components found in $INSTALL_PREFIX"
    else
        success "Uninstalled $removed component(s)"
    fi
    
    exit 0
}

# Handle uninstall
if [[ -n "$UNINSTALL" ]]; then
    uninstall
fi

# Detect platform
detect_platform() {
    local os arch
    
    # Detect OS
    case "$(uname -s)" in
        Linux*)
            os="linux"
            ;;
        Darwin*)
            os="darwin"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            error "Windows is not supported directly. Please use WSL (Windows Subsystem for Linux)"
            ;;
        *)
            error "Unsupported operating system: $(uname -s)"
            ;;
    esac
    
    # Detect architecture
    case "$(uname -m)" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            error "Unsupported architecture: $(uname -m)"
            ;;
    esac
    
    # Construct target triple
    if [[ "$os" == "linux" ]]; then
        # Prefer musl for better compatibility
        TARGET="${arch}-unknown-linux-musl"
    else
        TARGET="${arch}-apple-darwin"
    fi
    
    echo "$TARGET"
}

# Check dependencies
check_dependencies() {
    local missing=()
    
    for cmd in curl tar mkdir; do
        if ! command -v "$cmd" &> /dev/null; then
            missing+=("$cmd")
        fi
    done
    
    if [[ ${#missing[@]} -gt 0 ]]; then
        error "Missing required commands: ${missing[*]}"
    fi
}

# Get latest version from GitHub
get_latest_version() {
    local version
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')
    
    if [[ -z "$version" ]]; then
        error "Failed to fetch latest version from GitHub"
    fi
    
    echo "$version"
}

# Download and install
install() {
    info "Installing trx v$VERSION for $TARGET"
    
    # Create temporary directory
    local tmpdir
    tmpdir=$(mktemp -d -t trx-install.XXXXXX)
    trap "rm -rf '$tmpdir'" EXIT
    
    # Construct download URL
    local archive_name="trx-v${VERSION}-${TARGET}.tar.gz"
    local download_url="https://github.com/$REPO/releases/download/v${VERSION}/${archive_name}"
    
    info "Downloading from $download_url"
    
    # Download archive
    if ! curl -fsSL -o "$tmpdir/$archive_name" "$download_url"; then
        error "Failed to download trx v$VERSION for $TARGET. Please check if the version and platform are supported."
    fi
    
    success "Downloaded $archive_name"
    
    # Extract archive
    info "Extracting binaries..."
    if ! tar -xzf "$tmpdir/$archive_name" -C "$tmpdir"; then
        error "Failed to extract archive"
    fi
    
    # Create installation directory if it doesn't exist
    mkdir -p "$INSTALL_PREFIX"
    
    # Install all available components
    local installed=0
    local missing=()
    
    for component in "${COMPONENTS[@]}"; do
        local bin_path="$tmpdir/$component"
        
        if [[ -f "$bin_path" ]]; then
            # Check if component already exists
            if [[ -f "$INSTALL_PREFIX/$component" ]]; then
                warning "$component already exists, overwriting..."
            fi
            
            # Copy and make executable
            cp "$bin_path" "$INSTALL_PREFIX/$component"
            chmod +x "$INSTALL_PREFIX/$component"
            
            success "Installed $component"
            installed=$((installed + 1))
        else
            missing+=("$component")
        fi
    done
    
    if [[ $installed -eq 0 ]]; then
        error "No binaries found in archive. This might be a corrupted download."
    fi
    
    if [[ ${#missing[@]} -gt 0 ]]; then
        warning "Some components were not found: ${missing[*]}"
        info "These may not be included in this release version"
    fi
    
    success "Installed $installed component(s) to $INSTALL_PREFIX"
}

# Verify installation
verify_installation() {
    info "Verifying installation..."
    
    local verified=0
    for component in "${COMPONENTS[@]}"; do
        local bin_path="$INSTALL_PREFIX/$component"
        
        if [[ -f "$bin_path" && -x "$bin_path" ]]; then
            # Try to run --version if it's a main component
            if [[ "$component" == "trx" || "$component" == "trx-tui" ]]; then
                if version_output=$("$bin_path" --version 2>&1); then
                    success "$component: $version_output"
                    verified=$((verified + 1))
                else
                    warning "$component: installed but --version failed"
                fi
            else
                success "$component: installed"
                verified=$((verified + 1))
            fi
        fi
    done
    
    if [[ $verified -eq 0 ]]; then
        error "Installation verification failed. No working binaries found."
    fi
}

# Check if install directory is in PATH
check_path() {
    if [[ ":$PATH:" == *":$INSTALL_PREFIX:"* ]]; then
        success "$INSTALL_PREFIX is in your PATH"
        return 0
    else
        warning "$INSTALL_PREFIX is not in your PATH"
        echo
        echo "To use trx, add the following line to your shell configuration:"
        echo
        
        # Detect shell and provide appropriate instructions
        local shell_name
        shell_name=$(basename "$SHELL")
        
        case "$shell_name" in
            bash)
                echo "    echo 'export PATH=\"$INSTALL_PREFIX:\$PATH\"' >> ~/.bashrc"
                echo "    source ~/.bashrc"
                ;;
            zsh)
                echo "    echo 'export PATH=\"$INSTALL_PREFIX:\$PATH\"' >> ~/.zshrc"
                echo "    source ~/.zshrc"
                ;;
            fish)
                echo "    fish_add_path $INSTALL_PREFIX"
                ;;
            *)
                echo "    export PATH=\"$INSTALL_PREFIX:\$PATH\""
                echo
                echo "Add this to your shell's configuration file and restart your shell."
                ;;
        esac
        
        echo
        echo "Or run commands with full path: $INSTALL_PREFIX/trx"
        return 1
    fi
}

# Show next steps
show_next_steps() {
    echo
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}Installation complete!${NC}"
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo
    echo "Quick start:"
    echo
    echo "  1. Initialize trx in your project:"
    echo "     $ cd your-project"
    echo "     $ trx init"
    echo
    echo "  2. Create your first issue:"
    echo "     $ trx create \"My first task\" -t task"
    echo
    echo "  3. List open issues:"
    echo "     $ trx list"
    echo
    echo "  4. Launch the TUI viewer:"
    echo "     $ trx-tui"
    echo
    echo "For more information:"
    echo "  - Documentation: https://github.com/$REPO"
    echo "  - Run: trx --help"
    echo
    
    # Show uninstall instructions
    echo "To uninstall, run:"
    echo "  $ curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash -s -- --uninstall"
    echo
}

# Main installation flow
main() {
    echo
    echo -e "${BLUE}╔═══════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║${NC}  trx - Git-backed Issue Tracker      ${BLUE}║${NC}"
    echo -e "${BLUE}╚═══════════════════════════════════════╝${NC}"
    echo
    
    check_dependencies
    
    TARGET=$(detect_platform)
    success "Detected platform: $TARGET"
    
    # Resolve version
    if [[ "$VERSION" == "latest" ]]; then
        VERSION=$(get_latest_version)
        info "Latest version: $VERSION"
    fi
    
    install
    verify_installation
    
    local path_ok=0
    check_path || path_ok=$?
    
    show_next_steps
    
    if [[ $path_ok -ne 0 ]]; then
        exit 0  # Not an error, just a warning
    fi
}

# Run main function
main
