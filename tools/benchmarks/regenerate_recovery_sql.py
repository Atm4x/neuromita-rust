"""Synthetic SQL discovery comparison; not a Rust end-to-end benchmark."""
import json
import pathlib
import sqlite3
import statistics
import tempfile
import time
import tracemalloc
import uuid

N = 6000
RELATED = N // 2
UNRELATED = 40000
TRIALS = 60
PAGE = 128

def quantile(numbers, percentile):
    values = sorted(numbers)
    return values[min(len(values)-1, int(percentile * (len(values)-1)))]

with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory) / 'source.db'
    db = sqlite3.connect(path)
    db.executescript('''
    PRAGMA journal_mode=WAL;
    CREATE TABLE timeline_branches(branch_id TEXT PRIMARY KEY, session_id TEXT, parent_id TEXT, fork_sequence INTEGER, head_sequence INTEGER);
    CREATE TABLE timeline_events(branch_id TEXT NOT NULL, sequence INTEGER NOT NULL, fact_json TEXT NOT NULL, PRIMARY KEY(branch_id, sequence));
    CREATE INDEX timeline_regenerate_markers_by_character ON timeline_events(json_extract(fact_json,'$.draft.payload.regenerate_admission.character'),branch_id,sequence)
      WHERE json_extract(fact_json,'$.draft.payload.regenerate_admission.character') IS NOT NULL;
    ''')
    session=str(uuid.UUID(int=444))
    root=str(uuid.UUID(int=1000000))
    db.execute('INSERT INTO timeline_branches VALUES(?,?,?,?,?)', (root, session, None, 0, UNRELATED))
    db.executemany('INSERT INTO timeline_branches VALUES(?,?,?,?,?)', ((str(uuid.UUID(int=i+1)), session, root, 2, 3) for i in range(N)))
    db.executemany('INSERT INTO timeline_events VALUES(?,?,?)', ((str(uuid.UUID(int=i+1)), 3, json.dumps({'draft':{'kind':'BranchCreated','payload':{'regenerate_admission':{'character':'Crazy' if i%2==0 else 'Kind','request':str(uuid.UUID(int=i+20000))}}}})) for i in range(N)))
    db.executemany('INSERT INTO timeline_events VALUES(?,?,?)', ((root,i,json.dumps({'draft':{'kind':'PlayerMessage','payload':{'content':'unrelated'}}})) for i in range(1,UNRELATED+1)))
    db.commit()
    old='SELECT b.branch_id,e.fact_json FROM timeline_branches b JOIN timeline_events e ON e.branch_id=b.branch_id AND e.sequence=b.fork_sequence+1 WHERE b.session_id=? AND b.parent_id IS NOT NULL ORDER BY b.branch_id'
    new="SELECT e.branch_id,e.fact_json FROM timeline_events e INDEXED BY timeline_regenerate_markers_by_character JOIN timeline_branches b ON b.branch_id=e.branch_id WHERE json_extract(e.fact_json,'$.draft.payload.regenerate_admission.character')=? AND json_extract(e.fact_json,'$.draft.payload.regenerate_admission.character') IS NOT NULL AND e.branch_id>? AND b.session_id=? AND b.parent_id IS NOT NULL AND e.sequence=b.fork_sequence+1 ORDER BY e.branch_id LIMIT ?"
    def legacy():
        return [row[0] for row in db.execute(old, (session,)).fetchall() if json.loads(row[1])['draft']['payload']['regenerate_admission']['character']=='Crazy']
    def pages():
        after=''
        result=[]
        while True:
            fetched = db.execute(new, ('Crazy',after,session,PAGE+1)).fetchall()
            for branch, raw in fetched[:PAGE]:
                assert json.loads(raw)['draft']['payload']['regenerate_admission']['character']=='Crazy'
                result.append(branch)
            if len(fetched)<=PAGE:
                break
            after=fetched[PAGE-1][0]
        return result
    expected=legacy()
    assert len(expected)==RELATED and pages()==expected
    times={'old':[],'paged':[]}
    for i in range(TRIALS):
        for k, f in ([('old',legacy),('paged',pages)] if i%2==0 else [('paged',pages),('old',legacy)]):
            t=time.perf_counter()
            got=f()
            times[k].append((time.perf_counter()-t)*1000)
            assert got==expected
    allocation={}
    for key,f in [('old',legacy),('paged',pages)]:
        tracemalloc.start()
        tracemalloc.reset_peak()
        got=f()
        _, peak=tracemalloc.get_traced_memory()
        tracemalloc.stop()
        assert got==expected
        allocation[key]=peak/1024**2
    print('B73 synthetic SQL discovery comparison (Python sqlite3, NOT Rust end-to-end).')
    print(f'Rows: {N} fork markers ({RELATED} matching), {UNRELATED} unrelated facts, {TRIALS} iterations per method, page size {PAGE}.')
    for key in ['old','paged']:
        print(f'{key}: p50={statistics.median(times[key]):.3f} ms, p95={quantile(times[key],.95):.3f} ms, Python tracemalloc peak={allocation[key]:.3f} MiB, returned={RELATED}')
    print('SQL plans:')
    for key,query,args in [('old',old,(session,)),('paged',new,('Crazy','',session,PAGE+1))]:
        for item in db.execute('EXPLAIN QUERY PLAN '+query,args):
            print(f'  {key}: {item[3]}')
    print('Both timings return IDENTICAL full rows. Python heap peak excludes native SQLite allocations; results do not imply Rust end-to-end throughput or RSS.')
