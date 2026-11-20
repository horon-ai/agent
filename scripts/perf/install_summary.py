"""Render install artifacts in matrix order, including planned platforms without a report."""

import argparse
import html
import json
from pathlib import Path


def table_cell(text):
    text = ' '.join(str(text).split())
    if len(text) > 200:
        text = text[:70] + ' … ' + text[-125:]
    return '<code>' + html.escape(text).replace('|', '&#124;') + '</code>'


def write_platform(report, platform, tools, runs):
    complete = True
    for tool in tools:
        for state in ('cold', 'warm'):
            samples = [r for r in report['samples'] if r['tool'] == tool and r['state'] == state]
            attempts = [r for r in samples if not r.get('skipped')]
            success = sum('total_ms' in r for r in attempts)
            cell = report['summary_ms'].get(f'{tool}/{state}', {})
            total = cell.get('total_ms')
            times = ['–'] * 5
            if total:
                times = [f"{cell['install_ms']['median']:.1f}", f"{cell['first_version_ms']['median']:.1f}",
                         f"{total['median']:.1f}", f"{total['mad']:.1f}", f"{total['min']:.1f}–{total['max']:.1f}"]
            notes = []
            if len(samples) != runs:
                complete = False
                notes.append(f'{len(samples)} of {runs} planned results recorded')
            if len(attempts) != len(samples):
                notes.append(f'{len(samples) - len(attempts)} skipped')
            reason = next((r['error'] for r in samples if r.get('error')), None)
            if reason:
                notes.append(table_cell(reason))
            print('|', ' | '.join([platform, tool, state, f'{success}/{len(attempts)}', *times,
                                   '<br>'.join(notes) or '–']), '|')
    for error in report.get('cleanup_errors', []):
        print(f'| {platform} | – | cleanup | – | – | – | – | – | – | {table_cell(error)} |')
        complete = False
    return complete


def release_identity(report):
    """The jcode release record the platform installed, or the reason it is absent."""
    released = (report.get('releases') or {}).get('jcode')
    if released:
        return released
    for row in report.get('samples', []):
        if row.get('tool') == 'jcode' and row.get('release'):
            return row['release']
    for row in report.get('samples', []):
        if row.get('tool') == 'jcode' and row.get('error'):
            return {'error': row['error']}
    return None


def write_release(report, platform):
    released = release_identity(report)
    if not released:
        print(f'| {platform} | – | – | – | – | no jcode sample |')
        return
    if released.get('error'):
        print(f'| {platform} | – | – | – | – | {table_cell(released["error"])} |')
        return
    digest = released.get('sha256') or '–'
    print('|', ' | '.join([platform, released.get('tag') or '–', released.get('commit') or '–',
                            released.get('asset') or '–',
                            f'<code>{html.escape(str(digest))}</code>',
                            released.get('download_url') or '–']), '|')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--matrix', required=True, type=json.loads)
    parser.add_argument('--reports', required=True, type=Path)
    parser.add_argument('--runs', required=True, type=int)
    parser.add_argument('--competitors', required=True)
    parser.add_argument('--baseline-ref', default='')
    args = parser.parse_args()
    tools = ['horon']
    if args.baseline_ref:
        tools.append('horon-base')
    tools.extend(dict.fromkeys(name for name in args.competitors.split(',') if name))
    print('## Installation and first version output\n')
    print(f'{args.runs} cold/warm pairs planned per tool and platform. Times are milliseconds. '
          'Only successful samples contribute to timing statistics. Full diagnostics are in the platform artifacts.\n')
    print('| Platform | Tool | State | Success/attempted | Install median | First version median | '
          'Total median | Total MAD | Total range | Notes |')
    print('|---|---|---|---:|---:|---:|---:|---:|---:|---|')
    complete = True
    reports = {}
    for entry in args.matrix['include']:
        platform = entry['platform']
        path = args.reports / f'install-{platform}.json'
        try:
            report = json.loads(path.read_text(encoding='utf-8'))
            reports[platform] = report
            if not write_platform(report, platform, tools, args.runs):
                complete = False
        except (OSError, ValueError, KeyError, TypeError) as error:
            reason = 'Missing platform report' if isinstance(error, FileNotFoundError) else f'Unreadable platform report: {error}'
            print(f'| {platform} | – | – | unknown | – | – | – | – | – | {table_cell(reason)} |')
            complete = False
    if 'jcode' in tools:
        # The tag, commit, asset and digest are read from the sample's own record:
        # nothing here is inferred from this file or from the flag that pinned it.
        print('\n### jcode release identity\n')
        print('| Platform | Tag | Commit | Asset | SHA-256 | Download URL |')
        print('|---|---|---|---|---|---|')
        for entry in args.matrix['include']:
            platform = entry['platform']
            if platform in reports:
                write_release(reports[platform], platform)
            else:
                print(f'| {platform} | – | – | – | – | missing platform report |')
    return 0 if complete else 1


if __name__ == '__main__':
    raise SystemExit(main())
