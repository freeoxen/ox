#!/usr/bin/env bash
set -euo pipefail

# Coverage script for ox
# Uses cargo-llvm-cov for Rust and bun test --coverage for TypeScript

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
COVERAGE_DIR="$PROJECT_ROOT/target/coverage"
TS_UI_DIR="$PROJECT_ROOT/crates/ox-web/ui"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m' # No Color

# Default threshold for "needs coverage"
DEFAULT_THRESHOLD=80

# Whether each toolchain is available
HAS_LLVM_COV="false"
HAS_BUN="false"

usage() {
    cat <<EOF
Usage: $(basename "$0") [COMMAND] [OPTIONS]

Analyze and generate test coverage reports for ox (Rust + TypeScript).

Commands:
    (none)          Show summary with overall coverage and top gaps
    gaps            List files below threshold, sorted by uncovered lines
    file <path>     Show uncovered lines for a specific file
    html            Generate HTML report and open in browser (Rust only)
    lcov            Generate LCOV report(s) for CI integration
    json            Generate JSON report (Rust only)

Options:
    -h, --help          Show this help message
    -t, --threshold N   Coverage threshold percentage (default: $DEFAULT_THRESHOLD)
    -n, --top N         Show top N files (default: 10 for summary, all for gaps)
    --gate              Exit non-zero if coverage is below threshold
    --no-open           Don't open browser for HTML report
    --clean             Clean coverage data before running
    --skip-tests        Use cached coverage data (skip running tests)

Examples:
    $(basename "$0")                    # Summary with top 10 gaps
    $(basename "$0") gaps               # All files below ${DEFAULT_THRESHOLD}%
    $(basename "$0") gaps -t 90         # All files below 90%
    $(basename "$0") file src/lib.rs    # Uncovered lines in lib.rs
    $(basename "$0") html               # Open detailed HTML report
EOF
}

# Check for coverage toolchains
check_toolchains() {
    if command -v cargo-llvm-cov &> /dev/null; then
        HAS_LLVM_COV="true"
    else
        echo -e "${YELLOW}cargo-llvm-cov not found — skipping Rust coverage${NC}" >&2
        echo -e "${DIM}Install with: cargo install cargo-llvm-cov${NC}" >&2
    fi

    if command -v bun &> /dev/null && [[ -d "$TS_UI_DIR/src" ]]; then
        HAS_BUN="true"
    else
        echo -e "${YELLOW}bun not found or no TS source — skipping TypeScript coverage${NC}" >&2
    fi

    if [[ "$HAS_LLVM_COV" == "false" && "$HAS_BUN" == "false" ]]; then
        echo -e "${RED}No coverage toolchains available.${NC}"
        exit 1
    fi
}

# Clean coverage data
clean_coverage() {
    echo "Cleaning coverage data..."
    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        cargo llvm-cov clean --workspace
    fi
    rm -rf "$COVERAGE_DIR"
}

# ─── Rust coverage ───────────────────────────────────────────────────────────

ensure_rust_coverage_data() {
    local skip_tests="$1"
    [[ "$HAS_LLVM_COV" == "true" ]] || return 0

    local summary_file="$COVERAGE_DIR/rust_summary.txt"
    local detail_file="$COVERAGE_DIR/rust_detail.txt"

    if [[ "$skip_tests" == "true" && -f "$summary_file" && -f "$detail_file" ]]; then
        echo -e "${DIM}Using cached Rust coverage data${NC}" >&2
        return 0
    fi

    mkdir -p "$COVERAGE_DIR"
    echo -e "${DIM}Running Rust tests with coverage instrumentation...${NC}" >&2

    cargo llvm-cov --workspace 2>&1 | tee "$summary_file" | grep -E "^(running|test |TOTAL)" >&2 || true

    echo -e "${DIM}Generating detailed Rust coverage report...${NC}" >&2
    cargo llvm-cov report --text 2>&1 > "$detail_file"

    echo -e "${DIM}Rust coverage data cached.${NC}" >&2
}

get_rust_summary() {
    cat "$COVERAGE_DIR/rust_summary.txt" 2>/dev/null
}

get_rust_detail() {
    cat "$COVERAGE_DIR/rust_detail.txt" 2>/dev/null
}

# ─── TypeScript coverage ─────────────────────────────────────────────────────

ensure_ts_coverage_data() {
    local skip_tests="$1"
    [[ "$HAS_BUN" == "true" ]] || return 0

    local ts_summary_file="$COVERAGE_DIR/ts_summary.txt"

    if [[ "$skip_tests" == "true" && -f "$ts_summary_file" ]]; then
        echo -e "${DIM}Using cached TypeScript coverage data${NC}" >&2
        return 0
    fi

    mkdir -p "$COVERAGE_DIR"
    echo -e "${DIM}Running TypeScript tests with coverage...${NC}" >&2

    # bun test --coverage prints coverage table to stderr; capture everything
    (cd "$TS_UI_DIR" && bun test --coverage 2>&1) | tee "$ts_summary_file" | grep -E "^(✓|✗|pass|fail)" >&2 || true

    echo -e "${DIM}TypeScript coverage data cached.${NC}" >&2
}

get_ts_summary() {
    cat "$COVERAGE_DIR/ts_summary.txt" 2>/dev/null
}

# Parse the "All files" line from bun coverage table → prints line coverage %
# Returns empty string if not found.
get_ts_overall_pct() {
    get_ts_summary | awk -F'|' '
        /All files/ {
            # Field 3 is % Lines
            pct = $3
            gsub(/[[:space:]%]/, "", pct)
            print pct
        }
    '
}

# ─── Ensure all coverage data ────────────────────────────────────────────────

ensure_coverage_data() {
    local skip_tests="$1"
    ensure_rust_coverage_data "$skip_tests"
    ensure_ts_coverage_data "$skip_tests"
}

# ─── Display: summary ────────────────────────────────────────────────────────

color_pct() {
    local pct="$1"
    local threshold="$2"
    if (( $(echo "$pct < 50" | bc -l) )); then
        echo -n "$RED"
    elif (( $(echo "$pct < $threshold" | bc -l) )); then
        echo -n "$YELLOW"
    else
        echo -n "$GREEN"
    fi
}

show_summary() {
    local threshold="$1"
    local top_n="$2"
    local skip_tests="$3"
    local gate="$4"
    local failed=0

    ensure_coverage_data "$skip_tests"

    echo ""

    # ── Rust ──
    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        local rust_threshold
        if [[ "$gate" == "true" ]]; then
            rust_threshold=$(get_global_threshold "rust" "$threshold")
        else
            rust_threshold="$threshold"
        fi

        local rust_total
        rust_total=$(get_rust_summary | grep "^TOTAL" || true)

        if [[ -n "$rust_total" ]]; then
            local region_pct
            region_pct=$(echo "$rust_total" | awk '{print $4}' | tr -d '%')
            local pct_color
            pct_color=$(color_pct "$region_pct" "$rust_threshold")

            echo -e "${BOLD}Rust Coverage:${NC} ${pct_color}${region_pct}%${NC} ${DIM}(target: ${rust_threshold}%)${NC}"
            echo ""
            echo -e "${BOLD}Top Rust Coverage Gaps:${NC}"
            show_rust_gaps_internal "$rust_threshold" "$top_n" "true"

            if [[ "$gate" == "true" ]] && (( $(echo "$region_pct < $rust_threshold" | bc -l) )); then
                echo -e "${RED}FAIL: Rust overall ${region_pct}% below ${rust_threshold}% threshold${NC}"
                failed=1
            fi

            if [[ "$gate" == "true" ]]; then
                if ! check_rust_per_crate_thresholds "$rust_threshold"; then
                    failed=1
                fi
            fi
        fi
    fi

    # ── TypeScript ──
    if [[ "$HAS_BUN" == "true" ]]; then
        local ts_threshold
        if [[ "$gate" == "true" ]]; then
            ts_threshold=$(get_global_threshold "typescript" "$threshold")
        else
            ts_threshold="$threshold"
        fi

        local ts_pct
        ts_pct=$(get_ts_overall_pct)

        if [[ -n "$ts_pct" ]]; then
            local pct_color
            pct_color=$(color_pct "$ts_pct" "$ts_threshold")

            echo -e "${BOLD}TypeScript Coverage:${NC} ${pct_color}${ts_pct}%${NC} ${DIM}(target: ${ts_threshold}%)${NC}"
            echo ""
            echo -e "${BOLD}Top TypeScript Coverage Gaps:${NC}"
            show_ts_gaps_internal "$ts_threshold" "$top_n" "true"

            if [[ "$gate" == "true" ]] && (( $(echo "$ts_pct < $ts_threshold" | bc -l) )); then
                echo -e "${RED}FAIL: TypeScript overall ${ts_pct}% below ${ts_threshold}% threshold${NC}"
                failed=1
            fi

            if [[ "$gate" == "true" ]]; then
                if ! check_ts_per_file_thresholds "$ts_threshold"; then
                    failed=1
                fi
            fi
        fi
    fi

    return "$failed"
}

# ─── Per-crate threshold enforcement ─────────────────────────────────────────

COVERAGE_CONFIG="$PROJECT_ROOT/coverage.toml"

# Read a global threshold from [global] section.
# Usage: get_global_threshold rust|typescript [fallback]
get_global_threshold() {
    local lang="$1"
    local fallback="${2:-$DEFAULT_THRESHOLD}"
    [[ -f "$COVERAGE_CONFIG" ]] || { echo "$fallback"; return; }
    local val
    val=$(awk -F'=' -v lang="$lang" '
        /^\[global\]/ { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && /^[^#]/ {
            gsub(/[[:space:]]/, "", $1)
            gsub(/[[:space:]]/, "", $2)
            if ($1 == lang) { print $2; exit }
        }
    ' "$COVERAGE_CONFIG")
    echo "${val:-$fallback}"
}

# Parse a crate's threshold from coverage.toml.
# Returns the threshold, or empty string if not configured.
get_crate_threshold() {
    local crate="$1"
    [[ -f "$COVERAGE_CONFIG" ]] || return 0
    awk -F'=' -v crate="$crate" '
        /^\[rust\]/ { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && /^[^#]/ {
            gsub(/[[:space:]]/, "", $1)
            gsub(/[[:space:]]/, "", $2)
            if ($1 == crate) { print $2; exit }
        }
    ' "$COVERAGE_CONFIG"
}

# Check each Rust crate against its per-crate threshold.
# Falls back to the global threshold for unconfigured crates.
# Returns 0 if all pass, 1 if any fail.
check_rust_per_crate_thresholds() {
    local global_threshold="$1"
    local failed=0

    # Aggregate per-file coverage into per-crate coverage
    # by summing regions and missed regions per crate prefix.
    local crate_data
    crate_data=$(get_rust_summary | \
        grep -E "^[a-zA-Z0-9_-]+/" | \
        grep -v "^-" | \
        awk '{
            # file path is like ox-cli/src/main.rs
            split($1, parts, "/")
            crate = parts[1]
            regions = $2
            missed = $3
            crate_regions[crate] += regions
            crate_missed[crate] += missed
        }
        END {
            for (c in crate_regions) {
                total = crate_regions[c]
                missed = crate_missed[c]
                if (total > 0) {
                    pct = ((total - missed) / total) * 100
                } else {
                    pct = 100
                }
                printf "%s\t%.2f\n", c, pct
            }
        }')

    echo ""
    echo -e "${BOLD}Per-Crate Coverage:${NC}"

    echo "$crate_data" | sort | while IFS=$'\t' read -r crate pct; do
        local crate_thresh
        crate_thresh=$(get_crate_threshold "$crate")
        if [[ -z "$crate_thresh" ]]; then
            crate_thresh="$global_threshold"
        fi

        # Skip excluded crates (threshold = 0)
        if [[ "$crate_thresh" == "0" ]]; then
            printf "  ${DIM}%5.1f%%  %-20s (excluded)${NC}\n" "$pct" "$crate"
            continue
        fi

        local pct_color
        pct_color=$(color_pct "$pct" "$crate_thresh")

        if (( $(echo "$pct < $crate_thresh" | bc -l) )); then
            printf "  ${RED}%5.1f%%  %-20s FAIL (threshold: %s%%)${NC}\n" "$pct" "$crate" "$crate_thresh"
            # Signal failure to parent via temp file (subshell can't set parent vars)
            echo "1" > "$COVERAGE_DIR/.crate_gate_failed"
        else
            printf "  ${pct_color}%5.1f%%  %-20s (threshold: %s%%)${NC}\n" "$pct" "$crate" "$crate_thresh"
        fi
    done

    # Check if any crate failed (pipe subshell workaround)
    if [[ -f "$COVERAGE_DIR/.crate_gate_failed" ]]; then
        rm -f "$COVERAGE_DIR/.crate_gate_failed"
        echo ""
        echo -e "${RED}Per-crate coverage thresholds failed. See coverage.toml${NC}"
        return 1
    fi
    return 0
}

# Parse a TypeScript path's threshold from coverage.toml [typescript] section.
# Matches the longest prefix. Returns empty string if not configured.
get_ts_threshold() {
    local file="$1"
    [[ -f "$COVERAGE_CONFIG" ]] || return 0
    # Find the longest matching prefix
    awk -F'=' -v file="$file" '
        /^\[typescript\]/ { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && /^[^#]/ {
            gsub(/[[:space:]]/, "", $1)
            gsub(/[[:space:]]/, "", $2)
            prefix = $1
            if (index(file, prefix) == 1 && length(prefix) > best_len) {
                best_len = length(prefix)
                best_val = $2
            }
        }
        END { if (best_len > 0) print best_val }
    ' "$COVERAGE_CONFIG"
}

# Check each TypeScript file against its per-path threshold.
check_ts_per_file_thresholds() {
    local global_threshold="$1"
    local failed=0

    local file_data
    file_data=$(get_ts_summary | awk -F'|' '
        /^[[:space:]]*-/ { next }
        /File/ { next }
        /All files/ { next }
        /\.ts[[:space:]]*\|/ {
            file = $1
            pct = $3
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", file)
            gsub(/[[:space:]%]/, "", pct)
            if (file != "") printf "%s\t%s\n", file, pct
        }
    ')

    [[ -z "$file_data" ]] && return 0

    echo ""
    echo -e "${BOLD}Per-File TypeScript Coverage:${NC}"

    echo "$file_data" | sort | while IFS=$'\t' read -r file pct; do
        local file_thresh
        file_thresh=$(get_ts_threshold "$file")
        if [[ -z "$file_thresh" ]]; then
            file_thresh="$global_threshold"
        fi

        if [[ "$file_thresh" == "0" ]]; then
            printf "  ${DIM}%5.1f%%  %-40s (excluded)${NC}\n" "$pct" "$file"
            continue
        fi

        local pct_color
        pct_color=$(color_pct "$pct" "$file_thresh")

        if (( $(echo "$pct < $file_thresh" | bc -l) )); then
            printf "  ${RED}%5.1f%%  %-40s FAIL (threshold: %s%%)${NC}\n" "$pct" "$file" "$file_thresh"
            echo "1" > "$COVERAGE_DIR/.ts_gate_failed"
        else
            printf "  ${pct_color}%5.1f%%  %-40s (threshold: %s%%)${NC}\n" "$pct" "$file" "$file_thresh"
        fi
    done

    if [[ -f "$COVERAGE_DIR/.ts_gate_failed" ]]; then
        rm -f "$COVERAGE_DIR/.ts_gate_failed"
        echo ""
        echo -e "${RED}Per-file TypeScript thresholds failed. See coverage.toml${NC}"
        return 1
    fi
    return 0
}

# ─── Display: gaps ────────────────────────────────────────────────────────────

show_gaps() {
    local threshold="$1"
    local top_n="$2"
    local skip_tests="$3"

    ensure_coverage_data "$skip_tests"

    echo ""

    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        echo -e "${BOLD}Rust files below ${threshold}% coverage:${NC}"
        echo ""
        show_rust_gaps_internal "$threshold" "$top_n" "false"
    fi

    if [[ "$HAS_BUN" == "true" ]]; then
        echo -e "${BOLD}TypeScript files below ${threshold}% coverage:${NC}"
        echo ""
        show_ts_gaps_internal "$threshold" "$top_n" "false"
    fi
}

# Internal: parse and display Rust gaps
show_rust_gaps_internal() {
    local threshold="$1"
    local limit="$2"
    local compact="$3"

    local gap_data
    gap_data=$(get_rust_summary | \
        grep -E "^[a-zA-Z0-9_-]+/" | \
        grep -v "^-" | \
        awk -v threshold="$threshold" '
        {
            file = $1
            missed = $3
            pct = $4
            gsub(/%/, "", pct)

            if (pct + 0 >= threshold + 0) next
            printf "%d\t%.2f\t%s\n", missed, pct, file
        }' | \
        sort -t$'\t' -k1 -nr | \
        head -n "$limit")

    if [[ -z "$gap_data" ]]; then
        echo -e "  ${GREEN}All files above ${threshold}% threshold.${NC}"
    else
        echo "$gap_data" | while IFS=$'\t' read -r missed pct file; do
            local pct_color
            pct_color=$(color_pct "$pct" "$threshold")

            if [[ "$compact" == "true" ]]; then
                printf "  ${pct_color}%5.1f%%${NC}  %4d uncovered  %s\n" "$pct" "$missed" "$file"
            else
                printf "${pct_color}%6.2f%%${NC}  %5d uncovered  %s\n" "$pct" "$missed" "$file"
            fi
        done
    fi

    echo ""
}

# Internal: parse and display TypeScript gaps
# bun coverage table format (note: per-file lines have leading space):
#  ----------------------|---------|---------|-------------------
#  File                  | % Funcs | % Lines | Uncovered Line #s
#  ----------------------|---------|---------|-------------------
#  All files             |  100.00 |   99.72 |
#   src/dom-helpers.ts   |  100.00 |  100.00 |
#  ----------------------|---------|---------|-------------------
show_ts_gaps_internal() {
    local threshold="$1"
    local limit="$2"
    local compact="$3"

    local gap_data
    gap_data=$(get_ts_summary | awk -F'|' -v threshold="$threshold" '
        # Skip header, separator, and "All files" lines
        /^[[:space:]]*-/ { next }
        /File/ { next }
        /All files/ { next }
        # Match lines with a .ts file path (may have leading whitespace)
        /\.ts[[:space:]]*\|/ {
            file = $1
            pct = $3   # % Lines
            uncovered = $4

            gsub(/^[[:space:]]+|[[:space:]]+$/, "", file)
            gsub(/[[:space:]%]/, "", pct)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", uncovered)

            if (pct + 0 >= threshold + 0) next

            # Count uncovered lines from the line-number ranges
            n = split(uncovered, ranges, ",")
            missed = 0
            for (i = 1; i <= n; i++) {
                gsub(/[[:space:]]/, "", ranges[i])
                if (ranges[i] == "") continue
                if (index(ranges[i], "-") > 0) {
                    split(ranges[i], bounds, "-")
                    missed += bounds[2] - bounds[1] + 1
                } else {
                    missed += 1
                }
            }

            printf "%d\t%.2f\t%s\n", missed, pct, file
        }
    ' | \
        sort -t$'\t' -k1 -nr | \
        head -n "$limit")

    if [[ -z "$gap_data" ]]; then
        echo -e "  ${GREEN}All files above ${threshold}% threshold.${NC}"
    else
        echo "$gap_data" | while IFS=$'\t' read -r missed pct file; do
            local pct_color
            pct_color=$(color_pct "$pct" "$threshold")

            if [[ "$compact" == "true" ]]; then
                printf "  ${pct_color}%5.1f%%${NC}  %4d uncovered  %s\n" "$pct" "$missed" "$file"
            else
                printf "${pct_color}%6.2f%%${NC}  %5d uncovered  %s\n" "$pct" "$missed" "$file"
            fi
        done
    fi

    echo ""
}

# ─── Display: file detail ────────────────────────────────────────────────────

show_file() {
    local file_pattern="$1"
    local skip_tests="$2"

    ensure_coverage_data "$skip_tests"

    # Route to TS or Rust based on file extension
    if [[ "$file_pattern" == *.ts ]]; then
        show_ts_file "$file_pattern"
    else
        show_rust_file "$file_pattern"
    fi
}

show_rust_file() {
    local file_pattern="$1"

    local matching_files
    matching_files=$(get_rust_detail | grep -E "^$file_pattern:" | head -1 | cut -d: -f1 || true)

    if [[ -z "$matching_files" ]]; then
        matching_files=$(get_rust_detail | grep -E "^[^|]+$file_pattern[^|]*:" | head -1 | cut -d: -f1 || true)
    fi

    if [[ -z "$matching_files" ]]; then
        echo -e "${RED}No Rust coverage data found for: $file_pattern${NC}"
        echo "Try a more specific path or check that the file has tests."
        exit 1
    fi

    echo -e "${BOLD}Uncovered lines in:${NC} $matching_files"
    echo ""

    get_rust_detail | \
        awk -v file="$matching_files" '
            BEGIN { in_file = 0 }
            $0 ~ "^" file ":" { in_file = 1; next }
            /^[a-zA-Z_\/].*:$/ { if (in_file) exit }
            in_file && /^[ \t]+[0-9]+\|[ \t]+0\|/ {
                split($0, parts, "|")
                linenum = parts[1] + 0
                content = parts[3]
                for (i = 4; i <= length(parts); i++) content = content "|" parts[i]
                printf "\033[33m%4d\033[0m │%s\n", linenum, content
            }
        ' || true

    echo ""

    local file_summary
    file_summary=$(get_rust_summary | grep -F "$matching_files" | head -1 || true)
    if [[ -n "$file_summary" ]]; then
        local pct missed
        pct=$(echo "$file_summary" | awk '{print $4}')
        missed=$(echo "$file_summary" | awk '{print $3}')
        echo -e "${DIM}Coverage: $pct ($missed regions uncovered)${NC}"
    fi
}

show_ts_file() {
    local file_pattern="$1"

    # Find matching line in bun coverage table
    local file_line
    file_line=$(get_ts_summary | grep -F "$file_pattern" | grep '|' | head -1 || true)

    if [[ -z "$file_line" ]]; then
        echo -e "${RED}No TypeScript coverage data found for: $file_pattern${NC}"
        echo "Try a more specific path or check that the file has tests."
        exit 1
    fi

    local file pct_funcs pct_lines uncovered
    file=$(echo "$file_line" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/, "", $1); print $1}')
    pct_funcs=$(echo "$file_line" | awk -F'|' '{gsub(/[[:space:]%]/, "", $2); print $2}')
    pct_lines=$(echo "$file_line" | awk -F'|' '{gsub(/[[:space:]%]/, "", $3); print $3}')
    uncovered=$(echo "$file_line" | awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/, "", $4); print $4}')

    echo -e "${BOLD}Coverage for:${NC} $file"
    echo -e "${DIM}Functions: ${pct_funcs}%  Lines: ${pct_lines}%${NC}"
    echo ""

    if [[ -n "$uncovered" ]]; then
        echo -e "${BOLD}Uncovered lines:${NC} $uncovered"
    else
        echo -e "${GREEN}Full line coverage.${NC}"
    fi
}

# ─── Report generation ────────────────────────────────────────────────────────

generate_html() {
    local open_browser="$1"
    local skip_tests="$2"

    mkdir -p "$COVERAGE_DIR"

    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        if [[ "$skip_tests" != "true" ]]; then
            echo "Running Rust tests with coverage instrumentation..."
            cargo llvm-cov --workspace --html --output-dir "$COVERAGE_DIR"
        else
            echo "Generating Rust HTML from cached data..."
            cargo llvm-cov report --html --output-dir "$COVERAGE_DIR"
        fi
        echo -e "${GREEN}Rust HTML report: $COVERAGE_DIR/html/index.html${NC}"

        if [[ "$open_browser" == "true" ]]; then
            echo "Opening report in browser..."
            if [[ "$OSTYPE" == "darwin"* ]]; then
                open "$COVERAGE_DIR/html/index.html"
            elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
                xdg-open "$COVERAGE_DIR/html/index.html" 2>/dev/null || \
                    sensible-browser "$COVERAGE_DIR/html/index.html" 2>/dev/null || \
                    echo "Please open $COVERAGE_DIR/html/index.html in your browser"
            else
                echo "Please open $COVERAGE_DIR/html/index.html in your browser"
            fi
        fi
    fi

    if [[ "$HAS_BUN" == "true" ]]; then
        echo -e "${DIM}(TypeScript HTML coverage not yet supported — use 'lcov' for TS)${NC}"
    fi
}

generate_lcov() {
    local skip_tests="$1"
    mkdir -p "$COVERAGE_DIR"

    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        if [[ "$skip_tests" != "true" ]]; then
            echo "Running Rust tests with coverage instrumentation..."
            cargo llvm-cov --workspace --lcov --output-path "$COVERAGE_DIR/rust.lcov.info"
        else
            echo "Generating Rust LCOV from cached data..."
            cargo llvm-cov report --lcov --output-path "$COVERAGE_DIR/rust.lcov.info"
        fi
        echo -e "${GREEN}Rust LCOV report: $COVERAGE_DIR/rust.lcov.info${NC}"
    fi

    if [[ "$HAS_BUN" == "true" ]]; then
        echo "Running TypeScript tests with LCOV coverage..."
        mkdir -p "$COVERAGE_DIR/ts"
        (cd "$TS_UI_DIR" && bun test --coverage --coverage-reporter=lcov --coverage-dir "$COVERAGE_DIR/ts") 2>&1 | \
            grep -E "^(✓|✗|pass|fail)" >&2 || true
        echo -e "${GREEN}TypeScript LCOV report: $COVERAGE_DIR/ts/report.lcov${NC}"
    fi
}

generate_json() {
    local skip_tests="$1"
    mkdir -p "$COVERAGE_DIR"

    if [[ "$HAS_LLVM_COV" == "true" ]]; then
        if [[ "$skip_tests" != "true" ]]; then
            echo "Running Rust tests with coverage instrumentation..."
            cargo llvm-cov --workspace --json --output-path "$COVERAGE_DIR/coverage.json"
        else
            echo "Generating JSON from cached data..."
            cargo llvm-cov report --json --output-path "$COVERAGE_DIR/coverage.json"
        fi
        echo -e "${GREEN}Rust JSON report: $COVERAGE_DIR/coverage.json${NC}"
    fi

    if [[ "$HAS_BUN" == "true" ]]; then
        echo -e "${DIM}(TypeScript JSON coverage not yet supported — use 'lcov' for TS)${NC}"
    fi
}

# ─── Main ─────────────────────────────────────────────────────────────────────

main() {
    cd "$PROJECT_ROOT"

    local command=""
    local threshold="$DEFAULT_THRESHOLD"
    local top_n="10"
    local open_browser="true"
    local do_clean="false"
    local skip_tests="false"
    local gate="false"
    local file_arg=""

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            -h|--help)
                usage
                exit 0
                ;;
            -t|--threshold)
                threshold="$2"
                shift 2
                ;;
            -n|--top)
                top_n="$2"
                shift 2
                ;;
            --no-open)
                open_browser="false"
                shift
                ;;
            --clean)
                do_clean="true"
                shift
                ;;
            --skip-tests)
                skip_tests="true"
                shift
                ;;
            --gate)
                gate="true"
                shift
                ;;
            gaps|html|lcov|json)
                command="$1"
                shift
                ;;
            file)
                command="file"
                if [[ $# -lt 2 ]]; then
                    echo -e "${RED}Error: 'file' command requires a path argument${NC}"
                    exit 1
                fi
                file_arg="$2"
                shift 2
                ;;
            *)
                if [[ "$1" == *"/"* || "$1" == *".rs" || "$1" == *".ts" ]]; then
                    command="file"
                    file_arg="$1"
                    shift
                else
                    echo -e "${RED}Unknown option: $1${NC}"
                    usage
                    exit 1
                fi
                ;;
        esac
    done

    check_toolchains

    if [[ "$do_clean" == "true" ]]; then
        clean_coverage
    fi

    # Default top_n for gaps command
    if [[ "$command" == "gaps" && "$top_n" == "10" ]]; then
        top_n="100"
    fi

    case "$command" in
        ""|summary)
            show_summary "$threshold" "$top_n" "$skip_tests" "$gate"
            ;;
        gaps)
            show_gaps "$threshold" "$top_n" "$skip_tests"
            ;;
        file)
            show_file "$file_arg" "$skip_tests"
            ;;
        html)
            generate_html "$open_browser" "$skip_tests"
            ;;
        lcov)
            generate_lcov "$skip_tests"
            ;;
        json)
            generate_json "$skip_tests"
            ;;
    esac
}

main "$@"
