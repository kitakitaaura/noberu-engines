"""Compare luca-dump `scripts` output with LuckSystem `script decompile` output.

Both are one line per command. Jumps are normalized to command indexes:
LuckSystem prints labels, we print byte offsets.

  python3 -I diff_lucksystem.py <ours dir> <lucksystem dir>
"""
import os, re, sys
from collections import Counter

ours_dir, ref_dir = sys.argv[1], sys.argv[2]
PREFIX = re.compile(r'^((?:label|global)\d+: )+')
GOTO = re.compile(r'\{goto (?:"[^"]*" )?((?:label|global)\d+)\}')

# Global labels (FARCALL targets) span files: first pass collects them.
ref_lines, global_at = {}, {}
for fn in sorted(os.listdir(ref_dir)):
    name = fn[:-4]
    lines = open(os.path.join(ref_dir, fn), encoding='utf-8').read().split('\n')
    lines = [l for l in lines if l.strip()]
    out = []
    labels = {}
    for i, l in enumerate(lines):
        m = PREFIX.match(l)
        if m:
            for lab in re.findall(r'(label\d+|global\d+)', m.group(0)):
                if lab.startswith('global'):
                    global_at[lab] = (name.upper(), i)
                else:
                    labels[lab] = i
            l = l[m.end():]
        out.append(l)
    ref_lines[name.upper()] = (out, labels)

def norm_ref(l, labels):
    def sub(m):
        lab = m.group(1)
        if lab.startswith('global'):
            f, i = global_at.get(lab, ('?', -1))
            return '@%s' % i
        return '@%s' % labels.get(lab, -1)
    l = GOTO.sub(sub, l)
    return l

stats = Counter()
shown = Counter()
for fn in sorted(os.listdir(ours_dir)):
    name = fn[:-4].upper()
    if name not in ref_lines:
        stats['only ours: ' + name] += 1
        continue
    rlines, labels = ref_lines[name]
    ours = [l for l in open(os.path.join(ours_dir, fn), encoding='utf-8').read().split('\n') if l]
    pos_index = {}
    olines = []
    for i, l in enumerate(ours):
        p, rest = l.split(': ', 1)
        pos_index[int(p)] = i
        olines.append(rest)
    # FARCALL/JUMP targets are offsets in another file; map lazily below
    if len(olines) != len(rlines):
        stats['count mismatch'] += 1
        print('%s: %d ours vs %d LuckSystem' % (name, len(olines), len(rlines)))
    for i, (o, r) in enumerate(zip(olines, rlines)):
        r = norm_ref(r, labels)
        oname, rname = o.split(' ', 1)[0], r.split(' ', 1)[0]
        stats['lines'] += 1
        if oname != rname:
            stats['name differs'] += 1
            if shown['name'] < 10:
                shown['name'] += 1
                print('NAME %s[%d]: %s | %s' % (name, i, o[:80], r[:80]))
            continue
        if '[' in o.split(' ', 1)[-1][:1]:
            stats['raw (names match)'] += 1
            continue
        o = re.sub(r'\\u\{([0-9a-f]+)\}', lambda m: chr(int(m.group(1), 16)), o)
        o = o.replace('\\"', '"')  # LuckSystem leaves quotes inside strings unescaped
        o2 = re.sub(r'@(\d+)', lambda m: '@%s' % pos_index.get(int(m.group(1)), 'x' + m.group(1)), o)
        if oname in ('FARCALL', 'JUMP'):
            # cross-file targets: compare only the file name part
            o2 = re.sub(r', @\S+\)$', ')', o2); r2 = re.sub(r', @\S+\)$', ')', r)
            o2 = re.sub(r'^(\w+) \((\d+, )?', r'\1 (\2', o2)
        else:
            r2 = r
        # quoting: LuckSystem prints numbers bare and strings in Go quotes; ours Rust Debug
        if re.match(r'^(EQU|EQUN|ADD) \(\d+, \[', o2):
            # indexed write: LuckSystem prints the index and drops the value
            stats['indexed write (LuckSystem drops value)'] += 1
            continue
        if o2 == r2:
            stats['identical'] += 1
        else:
            stats['args differ: ' + oname] += 1
            if shown[oname] < 3:
                shown[oname] += 1
                print('ARGS %s[%d]:\n  ours %s\n  ref  %s' % (name, i, o2[:200], r2[:200]))
for k, v in sorted(stats.items()):
    print('%8d  %s' % (v, k))
