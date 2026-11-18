"""Writes the catalogue (catalogue/*.toml, catalogue/CREDITS.md) from what was read by hand
(sets.py, pins.txt, starter.txt, archive-bytes.txt beside this file), what was fetched
(WORK/sets/*.files, made by fetch.py) and what was measured (WORK/facts/*.jsonl, made by measure.py).

    python build.py SETS WORK            # into the repository this file is in
    python build.py SETS WORK ROOT       # into ROOT/catalogue, to look at before it is taken

SETS is the folder the sets were fetched into: their licence files are read there, and a quote that
is not in the licence file word for word stops the run. For whoever is choosing it also writes
WORK/out/everything-by-kind.md (every instrument with what was measured) and WORK/out/listening/*.list
(the starter selection a kind, for `sampled tour`). It fetches nothing. See README.md.
"""
import collections
import hashlib
import json
import os
import re
import sys

from sets import LEFT_OUT, SETS, SPLIT

HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) < 3:
    raise SystemExit(__doc__)
INSTRUMENTS, WORK = sys.argv[1], sys.argv[2]
ROOT = sys.argv[3] if len(sys.argv) > 3 else os.path.normpath(os.path.join(HERE, '..', '..', '..'))
FACTS = os.path.join(WORK, 'facts')
# The day the licences and the files were read: of the newest pin.
READ = '2026-11-17'
MB = 1 << 20


def listed(name):
    """The lines of a file beside this one that are not notes, split at their first space."""
    path = os.path.join(HERE, name)
    if not os.path.exists(path):
        return {}
    lines = [line.strip() for line in open(path, encoding='utf-8')]
    return dict(line.split(None, 1) for line in lines if line and not line.startswith('#'))


# The commit each set is pinned to, by its folder; and what each archive took when it was fetched
# and checked, by its set's id.
PINS = {name: rest.split()[-1] for name, rest in listed('pins.txt').items()}
ARCHIVE_BYTES = {name: int(size) for name, size in listed('archive-bytes.txt').items()}

# ---- what a licence file says ----------------------------------------------------------------

LICENCES = {
    'CC0-1.0': ('CC0 1.0 Universal',
                "Affirmer hereby overtly, fully, permanently, irrevocably and unconditionally waives, abandons, and "
                "surrenders all of Affirmer's Copyright and Related Rights and associated claims and causes of action"),
    'CC-BY-4.0': ('Attribution 4.0 International',
                  'the Licensor hereby grants You a worldwide, royalty-free, non-sublicensable, non-exclusive, '
                  'irrevocable license to exercise the Licensed Rights in the Licensed Material to: a. reproduce and '
                  'Share the Licensed Material, in whole or in part; and b. produce, reproduce, and Share Adapted Material'),
    'CC-BY-3.0': ('Attribution 3.0 Unported',
                  'Licensor hereby grants You a worldwide, royalty-free, non-exclusive, perpetual (for the duration of '
                  'the applicable copyright) license to exercise the rights in the Work as stated below: a. to Reproduce '
                  'the Work, to incorporate the Work into one or more Collections, and to Reproduce the Work as '
                  'incorporated in the Collections'),
    'Unlicense': ('This is free and unencumbered software released into the public domain.',
                  'Anyone is free to copy, modify, publish, use, compile, sell, or distribute this software'),
}
NOT_PASSING = ['ShareAlike', 'NonCommercial', 'NoDerivatives', 'Share Alike', 'Non-Commercial']


def squeezed(text):
    return ' '.join(text.split())


def licence_of(folder, files):
    """The licence file of a set, which licence it is, and the sentences of it that are quoted."""
    names = [path for path in files if '/' not in path and re.match(r'(?i)^(licen[cs]e|copying|unlicense)', path)]
    for name in sorted(names):
        text = squeezed(open(os.path.join(folder, name), encoding='utf-8', errors='replace').read())
        head = text[:400]
        for licence, (title, grant) in LICENCES.items():
            if title in head:
                if any(word in head for word in NOT_PASSING):
                    raise SystemExit(f'{folder}: {name} is a licence that does not pass')
                # The grant, as the file has it: found whatever the case of its first letter.
                at = text.lower().find(grant.lower())
                if at < 0:
                    raise SystemExit(f'{folder}: {name} is titled {title} and has not its grant')
                return name, licence, f'{title}. {text[at:at + len(grant)]}'
    return None


# ---- names and kinds -------------------------------------------------------------------------

SAID = {
    'ens': 'section', 'sus': 'sustained', 'vib': 'with vibrato', 'nv': 'without vibrato', 'vb': 'with vibrato',
    'stac': 'staccato', 'pizz': 'pizzicato', 'spic': 'spiccato', 'trem': 'tremolo', 'ks': 'keyswitched',
    'fhorn': 'French horn', 'sviolin': 'solo violin', 'exp': 'expressive', 'perc': 'percussion', 'keysw': 'keyswitched',
    'oh': 'overheads', 'rr': 'round robin', 'btb': 'behind the bridge', 'sxnth': 'sax synth', 'gm': 'General MIDI',
    'eguitar': 'Electric guitar',
}


def human(stem):
    """A file's name as a person would say it."""
    name = re.sub(r'^\d+\s*[-_]\s*', '', stem)
    # The day a file was made is not its name.
    name = re.sub(r'[-_ ]+20\d{6}$', '', name)
    name = re.sub(r'[_]+', ' ', name)
    name = re.sub(r'(?<=[a-z])(?=[A-Z])', ' ', name)
    name = re.sub(r'\s*-\s*', ' - ', name) if ' - ' in name else name.replace('-', ' ')
    words = [SAID.get(word.lower(), word) for word in name.split()]
    name = ' '.join(words).strip()
    # The cut-down version some sets bring of an instrument.
    if name.lower().endswith(' small'):
        name = name[:-len(' small')] + ', small set'
    return name[:1].upper() + name[1:]


SUSTAINS = {'strings', 'brass', 'woodwind', 'voice', 'organ', 'synth', 'effects'}
UNPITCHED = {'drum-kit', 'percussion', 'effects'}
BOWED = re.compile(r'(?i)arco|bow|sus|trem|legato|long|drone|accordion|harmonica|organ|pad')
# Rubbed and bowed things of any kind: their notes are held too.
RUBBED = re.compile(r'(?i)wine glass|bowed|friction')


def kind_of(entry, path, default, rules):
    lower = path.lower()
    for pattern, kind in rules:
        if re.search(pattern, lower):
            return kind
    return default


# ---- measured to marks -----------------------------------------------------------------------

# What a mark takes: see catalogue/README.md, which says the same in words.
NOISY_UNDER = 40.0       # dB: the quietest part of the middle recording, under its peak
TUNE_MIDDLE = 25.0       # cents: the middle key from equal temperament
TUNE_USUAL = 15.0        # cents: half the keys further than this from the middle one
EVEN_USUAL = 6.0         # dB: the usual step from one recorded key to the next
SILENT_UNDER = -60.0     # dB: the peak of all that was played
# Kinds whose tuning is given and not judged, and why.
NOT_JUDGED = {
    'mallets': 'a bar or a bell does not sound in harmonics, and the pitch read from its wave is not always the one that is heard',
    'voice': 'a voice wavers about its note',
}


def middle(values):
    values = sorted(values)
    half = len(values) // 2
    return values[half] if len(values) % 2 else (values[half - 1] + values[half]) / 2


def marks_of(kind, line, sustained):
    marks, notes = [], []
    heard = line['heard']
    if line['missing']:
        marks.append('incomplete')
        notes.append(f"{len(line['missing'])} of the {len(line['recordings'])} recordings its file names are not in its set")
    elif line.get('unreadable'):
        marks.append('incomplete')
        notes.append('Some of its recordings are files the player cannot read as audio, and their notes are silent')
    silent = 'failed' in heard or heard.get('peak_db', -200.0) < SILENT_UNDER
    if silent:
        marks.append('silent')
        woken = heard.get('woken')
        numbers = ', '.join(str(number) for number, _ in line['controllers'] if number != 64)
        if woken is not None and woken > SILENT_UNDER:
            notes.append(f'It says nothing until a controller is moved: with its own ({numbers}) at their middle it plays')
        elif 'failed' in heard:
            notes.append(f"It could not be played: {heard['failed']}")
    unpitched = kind in UNPITCHED or line['drums']
    noise = line.get('noise')
    if sustained or line.get('loops'):
        noise = None
        notes.append('Noise is not given: its notes are held or looped, and such a note never falls quiet')
    elif noise and noise[0] < NOISY_UNDER and not unpitched:
        # Judged only where keys are pitches: a struck sound is often cut while it still rings.
        marks.append('noisy')
    tuning = None
    cents = [off for _, off in line.get('cents') or []]
    if unpitched:
        notes.append('Tuning and evenness are not given: its keys are different sounds, not pitches of one')
    elif len(cents) >= 5:
        mid = middle(cents)
        tuning = (mid, middle([abs(off - mid) for off in cents]), max(cents, key=abs))
        if kind in NOT_JUDGED:
            notes.append(f'Its tuning is given and not judged: {NOT_JUDGED[kind]}')
        elif abs(tuning[0]) > TUNE_MIDDLE or tuning[1] > TUNE_USUAL:
            marks.append('out-of-tune')
    steps = None
    levels = [level for _, level in heard.get('levels') or []]
    # A key that said nothing is said apart, and is not a step of the level.
    sounding = [level for level in levels if level > -90.0]
    if not silent and len(sounding) < len(levels):
        notes.append(f'{len(levels) - len(sounding)} of the {len(levels)} keys that were played one by one said nothing')
    if not unpitched and not silent and len(sounding) >= 5:
        jumps = [abs(b - a) for a, b in zip(sounding, sounding[1:])]
        steps = (middle(jumps), max(jumps))
        if steps[0] > EVEN_USUAL:
            marks.append('uneven')
    return marks, notes, noise, tuning, steps


def unfaithful(line):
    """Whether what is not done is most of the sound: more than a quarter of what it plays is made, not recorded."""
    made = sum(un['count'] for un in line['unsupported'] if un['name'].startswith('sample=*'))
    return made * 4 > max(line['regions'], 1)


# ---- toml ------------------------------------------------------------------------------------

def quoted(text):
    return '"' + text.replace('\\', '\\\\').replace('"', '\\"') + '"'


def number(value, places=1):
    return f'{value:.{places}f}'


# ---- the sets --------------------------------------------------------------------------------

def files_of(name):
    files = collections.OrderedDict()
    for line in open(os.path.join(WORK, 'sets', name + '.files'), encoding='utf-8'):
        sha, size, path = line.rstrip('\n').split(' ', 2)
        files[path] = (sha, int(size))
    return files


def tree_sum(files):
    lines = sorted(files.items(), key=lambda item: item[0].encode('utf-8'))
    hasher = hashlib.sha256()
    for path, (sha, _) in lines:
        hasher.update(f'{sha}  {path}\n'.encode('utf-8'))
    return hasher.hexdigest()


def facts_of(name, skip):
    lines = [json.loads(line) for line in open(os.path.join(FACTS, name + '.jsonl'), encoding='utf-8')]
    # A file that could not be read at all has a line that says so and nothing else.
    lines = [line for line in lines if 'heard' in line]
    included = set()
    for line in lines:
        included.update(text for text in line['texts'] if text != line['file'])
    kept = []
    for line in lines:
        lower = '/' + line['file'].lower()
        if line['file'] in included or (skip and re.search(skip, lower)):
            continue
        if line['recordings'] and len(line['missing']) == len(line['recordings']):
            # Nothing it names is in its set: there is nothing to play, and nothing to offer.
            NOTHING_THERE.append(f"{name} {line['file']}")
            continue
        kept.append(line)
    return kept


SEEN = {}
NOTHING_THERE = []


def once(name, lines, files):
    """Without the instruments whose file is, to the byte, one that is listed already."""
    kept = []
    for line in sorted(lines, key=lambda line: line['file'].lower()):
        text = files[line['file']][0]
        if text in SEEN:
            print(f"the same file twice: {name} {line['file']} is {SEEN[text]}")
            continue
        SEEN[text] = f"{name} {line['file']}"
        kept.append(line)
    return kept


STARTER = {}


def instrument(line, files, set_info, default_kind, rules, alone, starter):
    path = line['file']
    stem = os.path.splitext(os.path.basename(path))[0]
    kind = kind_of(line, path, default_kind, rules)
    name = set_info['name'] if alone else human(stem)
    # A name its file does not say well, said by hand.
    for pattern, said in set_info.get('names', []):
        if re.search(pattern, path.lower()):
            name = said
            break
    words = set_info['about'] if alone else f"{name}. {set_info['about']}"
    # "Kick", "Natural", "Duo poly": a name that says nothing without its set is said with it.
    whole = set_info['name'].lower().replace(' ', '')
    if not alone and not set_info.get('part') and not any(len(word) >= 3 and word.lower() in whole for word in re.split(r'\W+', name)):
        name = f"{set_info['name']}: {name}"
    sustained = kind in SUSTAINS or (kind in ('bass', 'keyboard') and BOWED.search(stem) is not None) \
        or RUBBED.search(path) is not None
    marks, notes, noise, tuning, steps = marks_of(kind, line, sustained)
    if unfaithful(line):
        marks.append('unfaithful')
    if starter and re.search(starter, path.lower()):
        if marks:
            raise SystemExit(f"{set_info['id']} {path} is to be put first and has {marks} against it")
        marks = ['starter']
    out = [
        '[[set.instrument]]',
        f'file = {quoted(path)}',
        f'text = "{files[path][0]}"',
        f'name = {quoted(name)}',
        f'kind = "{kind}"',
        f'words = {quoted(words)}',
    ]
    if line['range']:
        out.append(f"range = [{line['range'][0]}, {line['range'][1]}]")
    if line['drums']:
        out.append('drums = true')
    out.append(f"layers = {line['layers']}")
    out.append(f"takes = {line['takes']}")
    out.append(f"recordings = {len(line['recordings'])}")
    out.append(f"bytes = {line['bytes']}")
    if line['ways']:
        out.append('ways = [' + ', '.join(quoted(way) for way in line['ways']) + ']')
    if noise:
        out.append(f'noise = [{number(min(noise[0], 150.0))}, {number(min(noise[1], 150.0))}]')
    if tuning:
        out.append(f'tuning = [{number(tuning[0])}, {number(tuning[1])}, {number(tuning[2])}]')
    if steps:
        out.append(f'evenness = [{number(steps[0])}, {number(steps[1])}]')
    if 'peak_db' in line['heard']:
        out.append(f"level = {number(max(line['heard']['peak_db'], -150.0))}")
    if marks:
        out.append('marks = [' + ', '.join(quoted(mark) for mark in marks) + ']')
    if line.get('says'):
        out.append(f"misses = {quoted(line['says'])}")
    if notes:
        out.append(f"note = {quoted('. '.join(notes) + '.')}")
    return dict(kind=kind, name=name, marks=marks, bytes=line['bytes'], text='\n'.join(out) + '\n', path=path, line=line,
                tuning=tuning, noise=noise, steps=steps)


def set_block(info, licence, licence_at, says, version, files, fetch_lines, instruments):
    total = sum(size for _, size in files.values())
    out = [
        '[[set]]',
        f"id = {quoted(info['id'])}",
        f"name = {quoted(info['name'])}",
        f"about = {quoted(info['about'] + '.')}",
        f"by = {quoted(info['by'])}",
        f'licence = "{licence}"',
        f'licence_at = {quoted(licence_at)}',
        f'licence_says = {quoted(says)}',
    ]
    if info.get('credit'):
        out.append(f"credit = {quoted(info['credit'])}")
    out += [
        f"from = {quoted(info['from'])}",
        f'version = "{version}"',
        f'read = "{READ}"',
        f'bytes = {total}',
        f'files = {len(files)}',
        '[set.fetch]',
    ] + fetch_lines + ['']
    return '\n'.join(out) + '\n' + '\n'.join(entry['text'] for entry in instruments) + '\n'


def main():
    blocks, lists, credits, everything, problems = [], [], [], [], []
    starter, also = listed('starter.txt'), listed('listen-also.txt')
    for info in SETS:
        name = info['dir']
        files = files_of(name)
        version = PINS[name]
        folder = os.path.join(INSTRUMENTS, name)
        found = licence_of(folder, files)
        if not found:
            raise SystemExit(f'{name}: no licence file')
        licence_file, licence, says = found
        if (info.get('credit') is not None) != licence.startswith('CC-BY'):
            raise SystemExit(f"{name}: it is {licence} and its credit line is {info.get('credit')!r}")
        repo = f"{info['owner']}/{name}"
        info['from'] = f'https://github.com/{repo}'
        licence_at = f'https://github.com/{repo}/blob/{version}/{licence_file}'
        lines = once(name, facts_of(name, info.get('skip')), files)
        if not lines:
            raise SystemExit(f'{name}: no instrument')
        entries = [instrument(line, files, info, info['kind'], info.get('rules', []), len(lines) == 1, starter.get(info['id']))
                   for line in sorted(lines, key=lambda line: line['file'].lower())]
        if info.get('by_file'):
            # An archive of it would not be the files as they are: it is fetched file by file.
            fetch = [f'base = "https://raw.githubusercontent.com/{repo}/{version}/"', f"list = {quoted(info['id'])}"]
            lists.append((info['id'], files))
        else:
            fetch = [f'archive = "https://github.com/{repo}/archive/{version}.zip"', f'under = "{name}-{version}/"',
                     f'sum = "{tree_sum(files)}"']
            # What the archive took the day it was fetched and checked: an estimate for the next time.
            if info['id'] in ARCHIVE_BYTES:
                fetch.append(f"archive_bytes = {ARCHIVE_BYTES[info['id']]}")
        blocks.append(set_block(info, licence, licence_at, says, version, files, fetch, entries))
        everything.append((info, licence, files, entries))
    for library in SPLIT:
        name = library['dir']
        all_files = files_of(name)
        version = PINS[name]
        repo = f"{library['owner']}/{name}"
        folder = os.path.join(INSTRUMENTS, name)
        found = licence_of(folder, all_files)
        if found:
            licence_file, licence, says = found
        else:
            # The library says its licence in its readme.
            licence_file, licence = 'README.md', 'CC0-1.0'
            says = 'This collection is under a Creative Commons 0 license.'
            text = squeezed(open(os.path.join(folder, licence_file), encoding='utf-8', errors='replace').read())
            if says not in text:
                raise SystemExit(f'{name}: its readme does not say what was quoted')
        licence_at = f'https://github.com/{repo}/blob/{version}/{licence_file}'
        lines = once(name, facts_of(name, None), all_files)
        taken = set()
        for part in library['parts']:
            mine = []
            for line in lines:
                key = line['file'].lower()
                if line['file'] not in taken and re.search(part['match'], key):
                    mine.append(line)
                    taken.add(line['file'])
            if not mine:
                raise SystemExit(f"{part['id']}: nothing matches")
            files = collections.OrderedDict()
            wanted = [licence_file] + [path for path in all_files if '/' not in path and path.lower().startswith('readme')]
            for line in mine:
                wanted += line['texts'] + line['recordings']
            for path in wanted:
                if path in all_files:
                    files[path] = all_files[path]
            info = dict(id=part['id'], name=part['name'], by=library['by'], about=f"{part['about']}. A part of the {library['library']}",
                        credit=None, dir=name, part=True)
            info['from'] = f"https://github.com/{repo}/tree/{library['branch']}"
            entries = [instrument(line, files, info, part['kind'], part.get('rules', []), False, starter.get(part['id']))
                       for line in sorted(mine, key=lambda line: line['file'].lower())]
            fetch = [f'base = "https://raw.githubusercontent.com/{repo}/{version}/"', f"list = {quoted(part['id'])}"]
            blocks.append(set_block(info, licence, licence_at, says, version, files, fetch, entries))
            lists.append((part['id'], files))
            everything.append((info, licence, files, entries))
        left = [line['file'] for line in lines if line['file'] not in taken]
        if left:
            problems.append(f'{name}: in no part: {left}')

    header = (
        '# The sampled instruments Trill may hand on. Written by a program from what was read, fetched and\n'
        '# measured on ' + READ + ': see catalogue/README.md for what each word means and how an entry is\n'
        '# added or brought up to date. Nothing here is a recording.\n\n'
        'format = 1\n\n'
    )
    catalogue = os.path.join(ROOT, 'catalogue')
    os.makedirs(catalogue, exist_ok=True)
    open(os.path.join(catalogue, 'instruments.toml'), 'w', encoding='utf-8', newline='\n').write(header + '\n'.join(blocks))
    with open(os.path.join(catalogue, 'files.toml'), 'w', encoding='utf-8', newline='\n') as out:
        out.write('# The files of the sets that are fetched file by file: a line each, its SHA-256, its size in bytes\n'
                  '# and its path below the set. See catalogue/README.md.\n\nformat = 1\n\n[list]\n')
        for name, files in lists:
            out.write(f"{name} = '''\n")
            for path, (sha, size) in files.items():
                if "'''" in path:
                    raise SystemExit(f'{name}: {path} cannot be written')
                out.write(f'{sha} {size} {path}\n')
            out.write("'''\n\n")
    with open(os.path.join(catalogue, 'left-out.toml'), 'w', encoding='utf-8', newline='\n') as out:
        out.write('# What was found and is not offered, with its terms as they were read on ' + READ + '.\n'
                  '# share-alike is listed apart for the owner to decide on; see catalogue/README.md.\n\nformat = 1\n\n')
        for name, at, reason, says in LEFT_OUT:
            out.write(f'[[left_out]]\nname = {quoted(name)}\nfrom = {quoted(at)}\nreason = "{reason}"\nsays = {quoted(says)}\n\n')
    with open(os.path.join(catalogue, 'CREDITS.md'), 'w', encoding='utf-8', newline='\n') as out:
        out.write('# Credits for the sampled instruments Trill offers\n\n'
                  'Trill ships no recordings. It offers sets of sampled instruments that their makers publish, which a\n'
                  'user fetches from where they are published. These makers ask to be named for theirs; the lines are\n'
                  'theirs to be shown wherever such a set is handed on, and wherever music made with one is released.\n'
                  'The other sets of the catalogue are dedicated to the public domain, and ask for nothing.\n\n'
                  'This list is written from `catalogue/instruments.toml`; a test holds the two to each other.\n\n')
        for info, licence, _, _ in sorted(everything, key=lambda item: item[0]['name'].lower()):
            if info.get('credit'):
                out.write(f"- {info['credit']}\n")

    # ---- for whoever is choosing: everything by kind, with what was measured -----------------
    by_kind = collections.defaultdict(list)
    for info, licence, files, entries in everything:
        size = sum(size for _, size in files.values())
        for entry in entries:
            by_kind[entry['kind']].append((info, licence, size, entry))
    order = ['piano', 'electric-piano', 'organ', 'keyboard', 'guitar', 'bass', 'strings', 'brass', 'woodwind', 'voice',
             'mallets', 'plucked', 'drum-kit', 'percussion', 'synth', 'effects']
    report = os.path.join(WORK, 'out')
    os.makedirs(report, exist_ok=True)
    counts = collections.Counter()
    with open(os.path.join(report, 'everything-by-kind.md'), 'w', encoding='utf-8', newline='\n') as out:
        total_sets = len(everything)
        total = sum(len(entries) for _, _, _, entries in everything)
        total_bytes = sum(sum(size for _, size in files.values()) for _, _, files, _ in everything)
        out.write(f'# Everything in the catalogue, by kind\n\n{total} instruments in {total_sets} sets, '
                  f'{total_bytes / (1 << 30):.1f} GB in all. Read and measured {READ}.\n\n'
                  'Marks: **starter** is fetched first; the others speak against an instrument, which is offered and not '
                  'put forward. Noise is how far under its peak the quietest part of a recording lies (the middle '
                  'recording, then the noisiest tenth). Tuning is cents from equal temperament: the middle key, how far '
                  'a key usually lies from that, the worst. Evenness is how the level steps from one recorded key to the '
                  'next through the player: the usual step, the most. An empty cell is a figure that would mean nothing: '
                  'the noise of a held note, the tuning of a kit.\n')
        for kind in order:
            rows = by_kind.get(kind, [])
            if not rows:
                continue
            out.write(f'\n## {kind} ({len(rows)})\n\n| Instrument | Set (size, licence) | Layers, takes | Noise dB | Tuning cents | Evenness dB | Marks | Not played |\n|---|---|---|---|---|---|---|---|\n')
            rows.sort(key=lambda row: ('starter' not in row[3]['marks'], bool(row[3]['marks']) and 'starter' not in row[3]['marks'], row[0]['name'].lower(), row[3]['name'].lower()))
            for info, licence, size, entry in rows:
                line = entry['line']
                counts[(kind, licence)] += 1
                noise = f"{entry['noise'][0]:.0f} / {entry['noise'][1]:.0f}" if entry['noise'] else ''
                tuning = f"{entry['tuning'][0]:+.0f} / {entry['tuning'][1]:.0f} / {entry['tuning'][2]:+.0f}" if entry['tuning'] else ''
                steps = f"{entry['steps'][0]:.1f} / {entry['steps'][1]:.1f}" if entry['steps'] else ''
                missed = ', '.join(sorted({re.sub(r'\d+', 'N', un['name']) for un in line['unsupported']}))[:70]
                marks = ', '.join(f'**{mark}**' if mark == 'starter' else mark for mark in entry['marks'])
                out.write(f"| {entry['name']} | {info['name']} ({size / MB:.0f} MB, {licence}) | {line['layers']}, {line['takes']} | {noise} | {tuning} | {steps} | {marks} | {missed} |\n")
    # ---- for whoever listens: the instruments that are put first, a list a kind for `sampled tour` ----
    PHRASE = {'piano': 'short-keys', 'electric-piano': 'short-keys', 'keyboard': 'short-keys', 'guitar': 'short-keys',
              'plucked': 'short-keys', 'mallets': 'short-keys', 'synth': 'short-keys', 'organ': 'short-chords',
              'strings': 'short-chords', 'voice': 'short-line', 'brass': 'short-line', 'woodwind': 'short-line',
              'bass': 'short-bass', 'drum-kit': 'short-drums', 'percussion': 'each', 'effects': 'each'}
    listening = os.path.join(report, 'listening')
    os.makedirs(listening, exist_ok=True)
    for old in os.listdir(listening):
        if old.endswith('.list'):
            os.remove(os.path.join(listening, old))
    known = {info['id'] for info, _, _, _ in everything}
    for name in list(starter) + list(also):
        if name not in known:
            raise SystemExit(f'starter.txt or listen-also.txt names a set that is not there: {name}')
    for position, kind in enumerate(order):
        def beside(info, entry):
            return info['id'] in also and re.search(also[info['id']], entry['path'].lower()) and 'starter' not in entry['marks']
        rows = [(info, entry) for info, _, _, entry in by_kind.get(kind, []) if 'starter' in entry['marks'] or beside(info, entry)]
        if not any('starter' in entry['marks'] for _, entry in rows):
            continue
        with open(os.path.join(listening, f'{position + 1:02d}-{kind}.list'), 'w', encoding='utf-8', newline='\n') as out:
            # What is put first, then what is played beside it.
            for info, entry in sorted(rows, key=lambda row: ('starter' not in row[1]['marks'], row[0]['name'].lower(), row[1]['name'].lower())):
                phrase = PHRASE[kind]
                # One player of a section's line: a solo instrument is given the line, not the chords.
                if phrase == 'short-chords' and kind == 'strings' and re.search(r'(?i)solo|cello|violin(?!s)|viola(?!s)|bass', entry['name']) \
                        and not re.search(r'(?i)section|ens', entry['name']):
                    phrase = 'short-line'
                where = os.path.abspath(os.path.join(INSTRUMENTS, info['dir'], entry['path'])).replace('\\', '/')
                called = entry['name'] if info['name'] in entry['name'] else f"{entry['name']} ({info['name']})"
                size = sum(size for _, size in next(files for other, _, files, _ in everything if other is info).values())
                called += f", {size / MB:.0f} MB"
                if 'starter' not in entry['marks']:
                    called += '  [not in the starter selection]'
                out.write(f"{phrase}\t{where}\t{called}\n")

    kinds = collections.Counter()
    licences = collections.Counter()
    sets_by_licence = collections.Counter()
    forward = collections.Counter()
    for info, licence, files, entries in everything:
        sets_by_licence[licence] += 1
        for entry in entries:
            kinds[entry['kind']] += 1
            licences[licence] += 1
            if not [mark for mark in entry['marks'] if mark != 'starter']:
                forward[entry['kind']] += 1
    print('sets', len(everything), 'instruments', sum(kinds.values()))
    print('by kind:', ', '.join(f'{kind} {kinds[kind]} ({forward[kind]} put forward)' for kind in order if kinds[kind]))
    print('by licence (sets, instruments):', {licence: (sets_by_licence[licence], licences[licence]) for licence in sets_by_licence})
    started = [(info['id'], sum(size for _, size in files.values())) for info, _, files, entries in everything
               if any('starter' in entry['marks'] for entry in entries)]
    print('starter:', [(name, round(size / MB)) for name, size in started], 'MB', round(sum(size for _, size in started) / MB))
    marks = collections.Counter(mark for _, _, _, entries in everything for entry in entries for mark in entry['marks'])
    print('marks:', dict(marks))
    for problem in problems:
        print('PROBLEM', problem)
    for nothing in NOTHING_THERE:
        print('nothing of it is there:', nothing)


main()
