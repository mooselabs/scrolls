#!/usr/bin/env python3
"""Executable generic workflow demonstration, also run in CI."""
import argparse, json, pathlib, subprocess
p=argparse.ArgumentParser()
p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/scrolls'))
p.add_argument('--out',type=pathlib.Path,required=True)
p.add_argument('--store',type=pathlib.Path)
a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
root=pathlib.Path(__file__).resolve().parents[1]
base=[str(a.binary.resolve()),'--json','--actor','alice','--role','human','--dolt',str(root/'.tools/dolt'),'--store',str(a.store or a.out/'store')]
transcript=[]
def call(*args):
    output=subprocess.run(base+list(map(str,args)),capture_output=True,text=True)
    assert output.returncode==0,(args,output.stdout,output.stderr)
    result=json.loads(output.stdout);transcript.append({'args':list(map(str,args)),'result':result});return result['data']
def batch(ops):
    file=a.out/f'batch-{len(transcript)}.json';file.write_text(json.dumps(ops));return call('batch',file)
call('init')
r=batch([
    {'op':'new','handle':'source','kind':'evidence','title':'Rollback measurement','body':'A killed transaction left zero partial rows.'},
    {'op':'new','handle':'doc','kind':'knowledge','title':'Atomic local writes','body':'Transactions preserve operation receipts with the write.'},
    {'op':'link','from':'$source','rel':'supports','to':'$doc'},
    {'op':'status','id':'$doc','state':'current'}])
k=r['handles']['doc'];call('review',r['branch']);call('review','accept',r['branch'],'--review-token',call('review',r['branch'])['review_token'])
q=call('new','question','--title','Keep the transaction boundary?','--body','Reviewing the result.')['id']
r=batch([{'op':'new','handle':'response','kind':'answer','title':'Yes','body':'The measured rollback is required.'},{'op':'link','from':'$response','rel':'answers','to':q,'expect_to':1}])
answer=r['handles']['response']
batch([{'op':'status','id':answer,'state':'accepted','expect':1},{'op':'status','id':q,'state':'answered','expect':2}])
context=call('context',q)
assert context['items'][0]['id']==q
assert any(x['id']==answer and 'body' not in x for x in context['items'][1:])
schema=call('schema','show');config=schema['schema'];config['pins']=[k,q,answer,k]
file=a.out/'schema.json';file.write_text(json.dumps(config))
proposal=call('schema','apply',file,'--expect',schema['schema_revision'])
call('review','accept',proposal['branch'],'--review-token',call('review',proposal['branch'])['review_token'])
packet=call('prime','--budget','1')
assert [x['id'] for x in packet['items']]==[k,q,answer]
content={key:packet[key] for key in ['guide','instructions','items']}
assert packet['used_bytes']==len(json.dumps(content,ensure_ascii=False,separators=(',',':')).encode())
assert packet['over_budget_bytes']>0
query=a.out/'query.json';query.write_text(json.dumps({'op':'all','args':[{'op':'compare','field':'kind','cmp':'eq','value':'question'},{'op':'compare','field':'owner','cmp':'eq','value':'$actor'}]}))
assert call('query','--file',query)['items'][0]['id']==q
call('walk',q,'--direction','incoming','--depth','1');call('history',q)
call('render','--format','native','--out',a.out/'rendered')
(a.out/'transcript.json').write_text(json.dumps(transcript,indent=2)+'\n')
print(f'Demonstrated schema review, atomic batches, owner guards, references, queries, pins and export: {a.out}')
