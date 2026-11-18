"""Measures and plays every instrument of every set that was fetched, through Trill's own player.

    python measure.py SETS WORK              # the sets that have no measurements yet
    python measure.py SETS WORK NAME...      # these again, by their folder's name
    python measure.py SETS WORK --again      # all of them again

For each set it runs `sampled facts` (the example of trill-engine) on SETS/<name> and writes
WORK/facts/<name>.jsonl: a line of JSON for every instrument file, with what its file says, the files it
is made of, what it asks for that the player does not do, how noisy, how well tuned and how even its
recordings are, and how loud a few notes of it come out. That is what an entry is written from.

The program that measures is built once and copied into WORK, so that it goes on being the same
program while a run lasts (all the sets take about a hundred minutes): `--tool PATH` names one that
is built already. Nothing runs this but a person: see README.md.
"""
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, '..', '..', '..'))


def tool(work, given):
    """The measuring program, as a copy in WORK."""
    copy = os.path.join(work, 'sampled-facts' + ('.exe' if os.name == 'nt' else ''))
    if given:
        shutil.copyfile(given, copy)
    elif not os.path.isfile(copy):
        subprocess.run(['cargo', 'build', '-p', 'trill-engine', '--example', 'sampled'], cwd=ROOT, check=True)
        built = os.path.join(ROOT, 'target', 'debug', 'examples', 'sampled' + ('.exe' if os.name == 'nt' else ''))
        shutil.copyfile(built, copy)
    if os.name != 'nt':
        os.chmod(copy, 0o755)
    return copy


def main():
    words = sys.argv[1:]
    given = None
    if '--tool' in words:
        at = words.index('--tool')
        given = words[at + 1]
        del words[at:at + 2]
    again = '--again' in words
    words = [word for word in words if word != '--again']
    if len(words) < 2:
        raise SystemExit(__doc__)
    sets, work, wanted = words[0], words[1], set(words[2:])
    facts = os.path.join(work, 'facts')
    os.makedirs(facts, exist_ok=True)
    program = tool(work, given)
    names = sorted(name[:-len('.files')] for name in os.listdir(os.path.join(work, 'sets')) if name.endswith('.files'))
    for name in names:
        out = os.path.join(facts, name + '.jsonl')
        if wanted and name not in wanted:
            continue
        if not wanted and not again and os.path.isfile(out) and os.path.getsize(out) > 0:
            continue
        folder = os.path.join(sets, name)
        if not os.path.isdir(folder):
            print(f'{name}: not fetched', flush=True)
            continue
        ran = subprocess.run([program, 'facts', folder, out + '.tmp'], capture_output=True, text=True)
        said = (ran.stdout.strip().splitlines() or ran.stderr.strip().splitlines() or ['it said nothing'])[-1]
        if ran.returncode == 0:
            os.replace(out + '.tmp', out)
            print(f'{name}: {said.split(":")[0]}', flush=True)
        else:
            print(f'{name}: FAILED: {said[:300]}', flush=True)


main()
