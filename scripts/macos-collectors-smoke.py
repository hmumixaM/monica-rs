#!/usr/bin/env python3
"""Synthetic, offline smoke test of the packaged interpreter and browser."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__);p.add_argument('runtime',type=Path);a=p.parse_args();root=a.runtime.resolve();python=root/'python/bin/python3.12'
with tempfile.TemporaryDirectory(prefix='monica-collector-fixture-') as temp:
 env={'PATH':'/usr/bin:/bin','HOME':temp,'PLAYWRIGHT_BROWSERS_PATH':str(root/'browsers'),'PYTHONNOUSERSITE':'1','PYTHONDONTWRITEBYTECODE':'1','MONICA_TEST_COLLECTOR_ROOT':str(root)}
 code="""from playwright.sync_api import sync_playwright
with sync_playwright() as p:
 browser=p.chromium.launch(headless=True,args=['--disable-background-networking','--disable-component-update','--no-first-run'])
 context=browser.new_context(offline=True)
 page=context.new_page();page.set_content('<title>Monica synthetic collector</title><main>Only synthetic data</main>')
 assert page.title()=='Monica synthetic collector'
 assert page.locator('main').inner_text()=='Only synthetic data'
 print('Bundled Chromium synthetic page passed: '+browser.version)
 browser.close()
"""
 subprocess.run([str(python),'-I','-B','-c',code],env=env,check=True)
 for provider in ['instagram','linkedin']:
  subprocess.run([str(root/'run-collector'),'--provider',provider,'--data-root',temp,'status'],env=env,check=True)
 for fixture in ['test-collector-bridge.py','test-collector-chatlog.py']:
  subprocess.run([str(python),'-B',str(Path(__file__).with_name(fixture))],env=env,check=True)
 print(json.dumps({'runtime':str(root),'result':'offline Chromium + isolated provider status + bundled publication/Chatlog module fixtures passed'}))
