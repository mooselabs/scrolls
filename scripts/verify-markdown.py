#!/usr/bin/env python3
"""Pinned Markdown-only oracle and workflow qualification, using only an owned SQL server."""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('scrolls_import_fixture', ROOT / 'scripts/verify-imports.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('repository', type=Path)
    parser.add_argument('--rev', required=True)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--git-web-url', default='https://github.com/proof-computer/liskov-agent-orchestrator')
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/scrolls')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    repo = args.repository.resolve()
    binary = args.binary.resolve()
    commit = fixture.run(['git', '-C', repo, 'rev-parse', '--verify', args.rev + '^{commit}']).decode().strip()
    entries = {}
    for row in fixture.run(['git', '-C', repo, 'ls-tree', '-rlz', '--full-tree', commit, '--', 'docs']).split(b'\0'):
        if not row:
            continue
        header, path = row.split(b'\t', 1)
        mode, kind, oid, size = header.decode().split()
        if path.endswith(b'.md'):
            assert mode in ('100644', '100755') and kind == 'blob'
            entries[path.decode()[5:]] = (mode, oid, int(size))
    assert entries, 'empty Markdown corpus'
    timings = {}
    with fixture.shared_server(out) as (connection, initial):
        settings = json.loads(connection.read_text())
        env = {k: v for k, v in os.environ.items() if not k.startswith(('DOLT_', 'GIT_'))}
        env.update(DOLT_ROOT_PATH=str(out / 'sql-client-home'), DOLT_DISABLE_EVENT_FLUSH='1')
        sql_base = [fixture.DOLT, '--host', '127.0.0.1', '--port', str(settings['port']), '--user', 'fixture', '--password', 'test-only', '--no-tls', '--use-db', settings['database'], 'sql', '-r', 'json', '-q']

        def sql(query):
            return json.loads(fixture.run(sql_base + [query], env=env))['rows']

        def call(*command, reader=False):
            base = [binary, '--json', '--actor', 'qualification', '--role', 'human', '--connection', connection, '--dolt', fixture.DOLT, '--store', out / ('reader' if reader else 'author')]
            result = json.loads(fixture.run(base + list(command)))
            assert result['ok'], result
            return result['data']

        init = call('init', '--dry-run')
        assert init['base_revision'] == initial
        call('init', '--op-id', 'initialize', '--expect-base', initial, '--expect-schema', init['ontology_digest'])
        plan = out / 'markdown.plan.json'
        start = time.monotonic()
        planned = call('import', 'plan', 'markdown', repo / 'docs', '--rev', commit, '--git-web-url', args.git_web_url, '--out', plan)
        timings['plan'] = time.monotonic() - start
        dry = call('import', 'apply', plan, '--op-id', 'markdown', '--dry-run')
        assert dry['sql_bytes'] <= dry['shared_query_bytes']
        start = time.monotonic()
        imported = call('import', 'apply', plan, '--op-id', 'markdown')
        timings['apply'] = time.monotonic() - start
        assert imported['import']['markdown'] == len(entries)
        rows = sql('SELECT id,payload FROM source_files ORDER BY id')
        assert {r['id'] for r in rows} == set(entries), 'selection mismatch or unexpected non-Markdown original'
        oracle = []
        git_env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        source = subprocess.Popen(['git', '--no-replace-objects', '-C', str(repo), 'cat-file', '--batch'], env=git_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            for row in rows:
                path = row['id']
                stored = json.loads(row['payload'])
                mode, oid, size = entries[path]
                source.stdin.write((oid + '\n').encode())
                source.stdin.flush()
                header = source.stdout.readline().decode().split()
                assert header == [oid, 'blob', str(size)]
                raw = source.stdout.read(size)
                assert source.stdout.read(1) == b'\n'
                assert base64.b64decode(stored['bytes']) == raw, path
                assert stored['git_oid'] == oid and stored['mode'] == int(mode, 8), path
                assert stored['sha256'] == hashlib.sha256(raw).hexdigest(), path
                oracle.append([path, mode, oid, stored['sha256']])
        finally:
            source.stdin.close()
            source.wait(timeout=10)
        assert source.returncode == 0
        print(f'Original oracle matched all {len(entries)} Markdown files', flush=True)
        original_digest = hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest()
        del rows
        doctor = call('doctor', reader=True)
        assert doctor['markdown_health']['files'] == len(entries)
        exported = out / 'export'
        start = time.monotonic()
        call('render', '--format', 'liskov', '--out', exported, reader=True)
        timings['render'] = time.monotonic() - start
        manifest = json.loads((exported / 'SCROLLS-MANIFEST.json').read_text())
        graph = json.loads((exported / 'scrolls/graph.json').read_text())
        imported_records = [r for r in graph['records'].values() if r['path'] is not None]
        assert {r['path'] for r in imported_records} == set(entries)
        for r in imported_records:
            expected = 'sc-' + hashlib.sha256(('import:docs/' + r['path']).encode()).hexdigest()[:32]
            assert r['id'] == expected and 'docs/' + r['path'] in r['aliases']
            assert bool((exported / r['path']).stat().st_mode & 0o111) == (entries[r['path']][0] == '100755')
        # Retained-link view changes are declared, originals are checked independently above.
        report = manifest['git_references']
        (out / 'references.json').write_text(json.dumps(report, indent=2) + '\n')
        for ref in report['rewritten']:
            assert '/' + commit + '/' in ref['url'] and ref['url'] in (exported / ref['document']).read_text()
        for ref in report['unresolved']:
            assert ref['target'] in (exported / ref['document']).read_text()
        knowledge = next(r for r in imported_records if r['kind'] == 'knowledge' and not r['frozen'])
        body = out / 'edit.md'
        body.write_text(knowledge['body'] + '\nQualification edit.\n')
        edited = call('edit', knowledge['id'], '--expect', str(knowledge['revision']), '--body-file', body, '--op-id', 'edit-imported')
        branch = edited['branch']
        assert branch.startswith('proposal/')
        review = call('review', branch, reader=True)
        call('review', 'accept', branch, '--review-token', review['review_token'], '--op-id', 'accept-edit', reader=True)
        call('new', 'note', '--title', 'Markdown qualification work note', '--body', 'New native authoring works.', '--op-id', 'native-note')
        decision = next(r for r in imported_records if r['kind'] == 'decision' and not r['frozen'])
        pending = call('edit', decision['id'], '--expect', str(decision['revision']), '--body', decision['body'] + '\nPending restored review.\n', '--op-id', 'pending-decision')
        token = call('review', pending['branch'], reader=True)['review_token']
        start = time.monotonic()
        backup = call('backup', 'create', '--out', out / 'backup')
        timings['backup_create_verify'] = time.monotonic() - start
        start = time.monotonic()
        restored = json.loads(fixture.run([binary, '--json', '--dolt', fixture.DOLT, 'backup', 'restore', out / 'backup', '--out', out / 'restored']))['data']
        timings['backup_restore_verify'] = time.monotonic() - start
        assert backup['snapshot_digest'] == restored['snapshot_digest'] and backup['verified'] and restored['verified']
        # Small integration fixtures separately restart this restored database and
        # accept its pending edit with the original token after deleting all source files.
        backup_manifest = json.loads((out / 'backup/verified.json').read_text())
        assert token and pending['branch'] in backup_manifest['snapshot']['branches']
        assert original_digest == hashlib.sha256(json.dumps(sql('SELECT id,payload FROM source_files ORDER BY id'), sort_keys=True).encode()).hexdigest()
        result = dict(source_commit=commit, tested_commit=fixture.run(['git', '-C', ROOT, 'rev-parse', 'HEAD']).decode().strip(), binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), files=len(entries), source_bytes=sum(e[2] for e in entries.values()), executable_markdown=sum(e[0] == '100755' for e in entries.values()), byte_mismatches=0, mode_mismatches=0, identity_mismatches=0, non_markdown_originals=0, source_inventory_digest=hashlib.sha256(json.dumps(oracle, separators=(',', ':')).encode()).hexdigest(), rewritten_references=len(report['rewritten']), unresolved_references=len(report['unresolved']), encoded_sql_bytes=dry['sql_bytes'], shared_query_bytes=dry['shared_query_bytes'], timings_seconds=timings, backup_snapshot_digest=backup['snapshot_digest'])
        (out / 'qualification.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)


if __name__ == '__main__':
    main()
