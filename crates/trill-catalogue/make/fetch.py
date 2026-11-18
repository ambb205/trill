"""Fetches the sets of the catalogue, each at the commit it is pinned to, for reading and measuring.

    python fetch.py SETS WORK            # every set of pins.txt that is not there yet
    python fetch.py SETS WORK NAME...    # these, by their folder's name
    python fetch.py --heads              # where each branch is now, beside its pin; fetches nothing else

SETS is a folder for the sets (28 GB for all of them); WORK is a folder for what is worked out of them.
Each set comes into SETS/<name> with its text files as its source has them (no line endings changed),
and WORK/sets/<name>.files gets a line "sha256 size path" for each of its files, hashed from the
commit's own blobs: what a fetch of the same commit from the source has, byte for byte, whatever a
working copy would make of it. The history is not kept.

It talks to github.com with git and says nothing of who runs it. It stops when the disk SETS is on
has less than 30 GB free. Nothing runs it but a person: see README.md.
"""
import hashlib
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
STOP_UNDER = 30 << 30
# No hooks, no line endings changed, nothing asked of whoever runs it.
GIT = ['git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.autocrlf=false', '-c', 'core.eol=lf',
       '-c', 'advice.detachedHead=false', '-c', 'init.defaultBranch=main']


def pins():
    """The sets: (folder, repository, branch or None, commit)."""
    found = []
    for line in open(os.path.join(HERE, 'pins.txt'), encoding='utf-8'):
        if line.strip() and not line.startswith('#'):
            name, repo, branch, commit = line.split()
            found.append((name, repo, None if branch == '-' else branch, commit))
    return found


def git(*words, cwd=None):
    return subprocess.run(GIT + list(words), cwd=cwd, check=True, capture_output=True).stdout


def blobs(clone):
    """(sha256, size, path) for every file of the commit a clone is at, read from the blobs."""
    listing = git('-c', 'core.quotepath=false', 'ls-tree', '-r', '-z', 'HEAD', cwd=clone)
    entries = []
    for record in listing.split(b'\0'):
        if record:
            meta, path = record.split(b'\t', 1)
            _, kind, blob = meta.split()
            if kind == b'blob':
                entries.append((blob, path.decode('utf-8')))
    reader = subprocess.Popen(GIT + ['cat-file', '--batch'], cwd=clone, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    for blob, path in entries:
        reader.stdin.write(blob + b'\n')
        reader.stdin.flush()
        size = int(reader.stdout.readline().split()[2])
        digest, left = hashlib.sha256(), size
        while left:
            piece = reader.stdout.read(min(left, 1 << 20))
            digest.update(piece)
            left -= len(piece)
        reader.stdout.read(1)
        yield digest.hexdigest(), size, path
    reader.stdin.close()
    reader.wait()


def fetch(name, repo, commit, sets, work):
    target = os.path.join(sets, name)
    shutil.rmtree(target, ignore_errors=True)
    os.makedirs(target)
    # The one commit, by its name: a branch may have moved since it was read.
    git('init', '--quiet', cwd=target)
    git('remote', 'add', 'origin', f'https://github.com/{repo}.git', cwd=target)
    # A repository that asks for its text files to be rewritten is told not to: the files as they are.
    os.makedirs(os.path.join(target, '.git', 'info'), exist_ok=True)
    with open(os.path.join(target, '.git', 'info', 'attributes'), 'w', encoding='utf-8', newline='\n') as out:
        out.write('* -text\n')
    git('fetch', '--quiet', '--depth', '1', 'origin', commit, cwd=target)
    git('checkout', '--quiet', 'FETCH_HEAD', cwd=target)
    lines = [f'{sha} {size} {path}\n' for sha, size, path in blobs(target)]
    os.makedirs(os.path.join(work, 'sets'), exist_ok=True)
    with open(os.path.join(work, 'sets', name + '.files'), 'w', encoding='utf-8', newline='\n') as out:
        out.writelines(lines)
    with open(os.path.join(work, 'sets', name + '.commit'), 'w', encoding='utf-8', newline='\n') as out:
        out.write(commit + '\n')
    shutil.rmtree(os.path.join(target, '.git'), ignore_errors=True)
    return len(lines)


def heads():
    moved = 0
    for name, repo, branch, commit in pins():
        said = subprocess.run(GIT + ['ls-remote', f'https://github.com/{repo}.git', branch or 'HEAD'],
                              capture_output=True, text=True).stdout.split()
        now = said[0] if said else 'not found'
        if now != commit:
            moved += 1
            print(f'{name}: pinned to {commit[:12]}, its branch is at {now[:12]}')
    print(f'{moved} of {len(pins())} have moved since they were read')


def main():
    words = sys.argv[1:]
    if words == ['--heads']:
        return heads()
    if len(words) < 2:
        raise SystemExit(__doc__)
    sets, work, wanted = words[0], words[1], set(words[2:])
    os.makedirs(sets, exist_ok=True)
    for name, repo, _, commit in pins():
        if wanted and name not in wanted:
            continue
        files = os.path.join(work, 'sets', name + '.files')
        pinned = os.path.join(work, 'sets', name + '.commit')
        there = os.path.isdir(os.path.join(sets, name)) and os.path.isfile(files) and os.path.isfile(pinned)
        if there and open(pinned).read().strip() == commit and not wanted:
            continue
        free = shutil.disk_usage(sets).free
        if free < STOP_UNDER:
            raise SystemExit(f'stopped before {name}: {free >> 30} GB are free on the disk')
        try:
            count = fetch(name, repo, commit, sets, work)
            print(f'{name}: {count} files at {commit[:12]}', flush=True)
        except subprocess.CalledProcessError as error:
            print(f'{name}: FAILED: {error.stderr.decode("utf-8", "replace").strip()[-300:]}', flush=True)


main()
