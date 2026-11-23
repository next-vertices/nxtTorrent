#!/usr/bin/env python3
"""Assemble une langue : keys.json (textes français, dans l'ordre) + <lang>.tsv ("index<TAB>traduction") -> src/locales/<lang>.json.

  python scripts/i18n-merge.py <lang> <keys.json> <tsv>

« = » dans le TSV signifie « identique au français » (noms propres, exemples, unités).
Pluriel : un texte français « une forme|autre forme » attend, dans la langue cible, ses formes CLDR séparées par « | » :
  en, es, it, pt : un|autre      ru : un|quelques|beaucoup|autre      zh : une seule forme
Les variables {x}, le gras ** et le code ` doivent être identiques au français.
"""
import json
import re
import sys

FORMS = {'en': ['one', 'other'], 'es': ['one', 'other'], 'it': ['one', 'other'], 'pt': ['one', 'other'],
         'ru': ['one', 'few', 'many', 'other'], 'zh': ['other']}
VAR = re.compile(r'\{(\w+)\}')


def main(lang, keys_path, tsv_path):
    keys = json.load(open(keys_path, encoding='utf-8'))
    tr = {}
    for ln, line in enumerate(open(tsv_path, encoding='utf-8').read().split('\n'), 1):
        if not line.strip():
            continue
        idx, _, text = line.partition('\t')
        if not idx.isdigit() or int(idx) >= len(keys):
            sys.exit(f'{tsv_path}:{ln}: index invalide {idx!r}')
        if int(idx) in tr:
            sys.exit(f'{tsv_path}:{ln}: index {idx} en double')
        tr[int(idx)] = text
    errors, out = [], {}
    missing = [i for i in range(len(keys)) if i not in tr]
    if missing:
        errors.append(f'{len(missing)} textes manquants, ex. {missing[:8]}')
    for i, key in enumerate(keys):
        if i not in tr:
            continue
        text = tr[i]
        plural = '|' in key
        if text == '=':
            val = key if not plural else key
        else:
            val = text
        want = set(VAR.findall(key))
        if plural:
            forms = val.split('|')
            need = len(FORMS[lang])
            if text == '=':
                forms = key.split('|')
                forms = (forms + [forms[-1]] * need)[:need] if need > 2 else forms
            if len(forms) != need:
                errors.append(f'#{i} {key[:50]!r}: {len(forms)} formes au lieu de {need}')
                continue
            for f in forms:
                if set(VAR.findall(f)) - want:
                    errors.append(f'#{i} {key[:50]!r}: variables {sorted(set(VAR.findall(f)))} vs {sorted(want)} dans {f[:60]!r}')
            if lang == 'zh':
                out[key] = forms[0]
            elif lang in ('en', 'es', 'it', 'pt'):
                out[key] = {'one': forms[0], 'other': forms[1]}
            else:
                out[key] = dict(zip(FORMS[lang], forms))
            continue
        if set(VAR.findall(val)) != want:
            errors.append(f'#{i} {key[:50]!r}: variables {sorted(set(VAR.findall(val)))} au lieu de {sorted(want)}')
        if val.count('**') != key.count('**') or val.count('`') != key.count('`'):
            errors.append(f'#{i} {key[:50]!r}: ** ou ` différents ({val.count("**")}/{key.count("**")}, {val.count(chr(96))}/{key.count(chr(96))})')
        out[key] = val
    if errors:
        print('\n'.join(errors[:60]))
        sys.exit(f'{lang}: {len(errors)} erreur(s)')
    # les entrées identiques au français restent dans le fichier : le test de couverture distingue « oublié » de « inchangé »
    out = dict(sorted(out.items()))
    with open(f'src/locales/{lang}.json', 'w', encoding='utf-8', newline='\n') as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
        f.write('\n')
    print(f'{lang}: {len(out)} entrées')


if __name__ == '__main__':
    main(*sys.argv[1:4])
