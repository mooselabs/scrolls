#!/usr/bin/env python3
"""Qualify a pinned PEP snapshot through the public CLI and raw Git objects."""
import argparse, hashlib, json, pathlib, subprocess, os
p=argparse.ArgumentParser()
p.add_argument('repository',type=pathlib.Path)
p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/scrolls'))
p.add_argument('--out',type=pathlib.Path,required=True)
a=p.parse_args(); a.out.mkdir(parents=True,exist_ok=False)
repo=a.repository.resolve(); root=pathlib.Path(__file__).resolve().parents[1]
rev='6068058b896441388bbf22f2607ab681193f6a3d'
base=[str(a.binary.resolve()),'--json','--actor','qualification','--role','human','--dolt',str(root/'.tools/dolt'),'--store',str(a.out/'store')]
def run(args):
 r=subprocess.run(list(map(str,args)),capture_output=True,check=True)
 return r.stdout
def call(*args):
 v=json.loads(run(base+list(args)));assert v['ok'],v;return v['data']
call('init')
plan=a.out/'peps.plan.json'
planned=call('import','plan','git',repo,'--root','peps','--rev',rev,'--source','python.peps','--profile','pep','--out',plan)
coverage=planned['coverage'];print(json.dumps(coverage,indent=2),flush=True)
for key,value in {'documents':744,'files':820,'header_references':113,'edges':46,'unmapped_references':40,'unresolved_references':0}.items(): assert coverage[key]==value,(key,coverage[key])
call('import','apply',plan,'--op-id','peps-initial','--dry-run')
result=call('import','apply',plan,'--op-id','peps-initial')
assert result==call('import','apply',plan,'--op-id','peps-initial')
record=call('show','PEP-0008','--provenance');assert record['record']['kind']=='evidence'
assert record['provenance']['membership']['metadata']['pep']=='8'
assert call('lint')['errors']==[]
export=a.out/'export';call('render','--out',export)
entries=run(['git','-C',repo,'ls-tree','-r','-z',rev+':peps']).split(b'\0')
checks=[]
for entry in filter(None,entries):
 header,path=entry.split(b'\t',1);mode,kind,oid=header.decode().split();assert kind=='blob'
 raw=run(['git','-C',repo,'cat-file','blob',oid]);file=export/'sources/python.peps'/path.decode()
 assert file.read_bytes()==raw,path
 assert bool(file.stat().st_mode&0o111)==(mode=='100755'),path
 checks.append([path.decode(),mode,oid,hashlib.sha256(raw).hexdigest()])
report={'source_commit':rev,'tested_commit':run(['git','-C',root,'rev-parse','HEAD']).decode().strip(),'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'coverage':coverage,'candidate_digest':result['candidate_digest'],'source_inventory_digest':hashlib.sha256(json.dumps(checks,separators=(',',':')).encode()).hexdigest(),'byte_mismatches':0,'mode_mismatches':0}
(a.out/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
