import hashlib
import json
from pathlib import Path
import subprocess


def canonical_hash(data):
    return hashlib.sha256(data.replace(b'\r\n', b'\n')).hexdigest()


root = Path(__file__).resolve().parents[3]
manifest = json.loads(Path(__file__).with_name('build-inputs.json').read_text())
paths = [item['path'] for item in manifest['files']]
assert len(paths) == len(set(paths)), 'Duplicate implementation records'
for item in manifest['files']:
    current = (root / item['path']).read_bytes()
    assert canonical_hash(current) == item['canonical_sha256'], item['path']
    original = subprocess.check_output(
        ['git', 'show', f"{manifest['implementation_commit']}:{item['path']}"],
        cwd=root,
    )
    assert canonical_hash(original) == item['canonical_sha256'], item['path']
for item in manifest['post_build_documentation']:
    assert item['path'] not in paths, 'Documentation duplicated as unchanged input'
    original = subprocess.check_output(
        ['git', 'show', f"{manifest['implementation_commit']}:{item['path']}"],
        cwd=root,
    )
    assert canonical_hash(original) == item['implementation_commit_snapshot']['canonical_sha256'], item['path']
    current = (root / item['path']).read_bytes()
    assert canonical_hash(current) == item['current_documentation_snapshot']['canonical_sha256'], item['path']
for item in manifest['post_build_packaging']:
    assert item['path'] not in paths, 'Changed packaging recorded as unchanged input'
    original = subprocess.check_output(
        ['git', 'show', f"{manifest['implementation_commit']}:{item['path']}"],
        cwd=root,
    )
    assert canonical_hash(original) == item['implementation_commit_snapshot']['canonical_sha256'], item['path']
    current = (root / item['path']).read_bytes()
    assert canonical_hash(current) == item['current_packaging_snapshot']['canonical_sha256'], item['path']
    packaged = subprocess.check_output(
        ['git', 'show', f"{manifest['packaging_commit']}:{item['path']}"],
        cwd=root,
    )
    assert canonical_hash(packaged) == item['current_packaging_snapshot']['canonical_sha256'], item['path']
print(json.dumps({
    'implementation_commit': manifest['implementation_commit'],
    'unchanged_canonical_records_verified': len(paths),
    'post_build_documentation_snapshots_verified': len(manifest['post_build_documentation']),
    'packaging_commit': manifest['packaging_commit'],
    'post_build_packaging_snapshots_verified': len(manifest['post_build_packaging']),
}))
