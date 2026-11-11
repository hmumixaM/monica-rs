#!/usr/bin/env python3
"""Deterministic synthetic records; never opens real archives or credentials."""
import argparse,json
p=argparse.ArgumentParser();p.add_argument('--records',type=int,default=1000);a=p.parse_args()
if a.records < 1: p.error('records must be positive')
print(json.dumps([{'provider':['wechat','instagram','linkedin'][i%3], 'account':f'person-{i%100}', 'object':f'post-{i}', 'event':f'event-{i}', 'payload':{'text':f'Synthetic fixture {i}'}} for i in range(a.records)]))
