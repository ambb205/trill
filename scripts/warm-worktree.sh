#!/usr/bin/env sh
# Gives a new git worktree a head start: copies the libraries the main
# checkout has already built, so that the first build here compiles only
# Trill's own crates (about a minute and a half instead of four).
#
#   sh scripts/warm-worktree.sh        # from the root of the new worktree
#
# Does nothing in the main checkout, or where something is built already.
# Run it while the main checkout is not in the middle of a build.
set -eu

here=$(git rev-parse --show-toplevel)
main=$(git worktree list --porcelain | sed -n '1s/^worktree //p')
profile=${1:-debug}

if [ "$here" = "$main" ]; then
    echo "this is the main checkout; nothing to copy"
    exit 0
fi
if [ ! -d "$main/target/$profile" ]; then
    echo "the main checkout ($main) has no target/$profile to copy; build there first"
    exit 0
fi
if [ -d "$here/target/$profile" ]; then
    echo "target/$profile exists here already; leaving it alone"
    exit 0
fi

mkdir -p "$here/target/$profile"
# Not the incremental caches: they belong to the main checkout's own crates,
# which are rebuilt here under other names anyway.
if command -v robocopy >/dev/null 2>&1; then
    # Robocopy's exit codes below 8 all mean success.
    MSYS2_ARG_CONV_EXCL='*' robocopy "$(cygpath -w "$main/target/$profile")" "$(cygpath -w "$here/target/$profile")" \
        /E /MT:16 /XD incremental /NFL /NDL /NJH /NJS /NP >/dev/null || [ $? -lt 8 ]
else
    (cd "$main/target/$profile" && find . -path ./incremental -prune -o -print | cpio -pdm --quiet "$here/target/$profile")
fi
echo "copied $main/target/$profile; the first build here now compiles only Trill's own crates"
