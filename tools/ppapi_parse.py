#!/usr/bin/env python3
"""Parse PPAPI C headers into interface descriptions (shared by gen-ppapi.py)."""
import re, glob, os, sys

ROOT = os.path.join(os.path.dirname(__file__), '..', 'third_party', 'ppapi', 'c')

def strip_comments(s):
    s = re.sub(r'/\*.*?\*/', '', s, flags=re.S)
    s = re.sub(r'//[^\n]*', '', s)
    return s

def load():
    texts = {}
    for p in sorted(glob.glob(ROOT + '/**/*.h', recursive=True)):
        texts[p] = strip_comments(open(p, encoding='utf-8', errors='replace').read())
    return texts

DEFINE = re.compile(r'#define\s+(PP[BP]_\w+_INTERFACE_\w+)\s+(?:"([^"]+)"|(PP[BP]_\w+_INTERFACE_\w+))')
STRUCT = re.compile(r'struct\s+(PP[BP]_\w+)\s*\{(.*?)\n\};', re.S)
MEMBER = re.compile(r'^\s*(.+?)\s*\(\s*\*\s*(\w+)\s*\)\s*\((.*?)\)\s*;', re.S | re.M)

def split_args(a):
    a = a.strip()
    if a in ('', 'void'):
        return []
    out, depth, cur = [], 0, ''
    for ch in a:
        if ch == '(': depth += 1
        if ch == ')': depth -= 1
        if ch == ',' and depth == 0:
            out.append(cur.strip()); cur = ''
        else:
            cur += ch
    out.append(cur.strip())
    return out

def parse_arg(a):
    a = ' '.join(a.split())
    a = re.sub(r'\bconst\b', '', a).strip()
    m = re.match(r'^(.*?)(\w+)((?:\[\d*\])*)$', a)
    # type = everything before the final identifier (pointer stars stay with the type)
    if '(' in a:  # function pointer parameter
        return ('fnptr', 'callback')
    m = re.match(r'^(.*[\s\*])(\w+)(\[\d*\])?$', a)
    if not m:
        return (a.strip(), '')
    t, n = m.group(1).strip(), m.group(2)
    if m.group(3):
        t += '*'
    return (' '.join(t.split()), n)

def interfaces():
    texts = load()
    names = {}   # define -> string
    raw = []
    for p, t in texts.items():
        for m in DEFINE.finditer(t):
            raw.append(m.groups())
    for d, s, alias in raw:
        if s: names[d] = s
    for d, s, alias in raw:
        if alias and alias in names: names[d] = names[alias]
    structs = {}
    for p, t in texts.items():
        for m in STRUCT.finditer(t):
            name, body = m.group(1), m.group(2)
            meths = []
            for mm in MEMBER.finditer(body):
                ret, fn, args = mm.group(1).strip(), mm.group(2), mm.group(3)
                ret = ' '.join(re.sub(r'\bconst\b', '', ret).split())
                meths.append({'name': fn, 'ret': ret, 'args': [parse_arg(a) for a in split_args(args)]})
            if meths:
                structs[name] = {'methods': meths, 'file': os.path.relpath(p, ROOT)}
    # interface string -> struct name, from every quoted "PPB_Name(Suffix);maj.min" literal in the headers
    by_string = {}
    lits = set()
    for p, t in texts.items():
        for m in re.finditer(r'"(PP[BP]_[\w()]+;[\d.]+)"', t):
            lits.add(m.group(1))
    for sstr in sorted(lits):
        name, ver = sstr.split(';')
        name2 = re.sub(r'\((\w+)\)', r'_\1', name)
        vtag = ver.replace('.', '_')
        cands = [f'{name2}_{vtag}', f'{name2}_{vtag}_0' if '.' not in ver else '', name2]
        # e.g. "PPB_Flash_File_ModuleLocal;3" -> PPB_Flash_File_ModuleLocal_3_0 or PPB_Flash_File_ModuleLocal
        for c in cands:
            if c and c in structs:
                by_string[sstr] = c
                break
    return structs, by_string

if __name__ == '__main__':
    structs, by_string = interfaces()
    print(len(structs), 'structs', len(by_string), 'interface strings')
    types = {}
    for sn, st in structs.items():
        for m in st['methods']:
            types.setdefault(m['ret'], set()).add(sn)
            for t, n in m['args']:
                types.setdefault(t, set()).add(sn)
    for t in sorted(types):
        print(f'{t!r:60} {len(types[t])}')
