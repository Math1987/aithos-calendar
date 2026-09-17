#!/usr/bin/env python3
"""Isolated browser preview with fake API responses; never calls AWS or Google."""
import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--port', type=int, default=3189)
parser.add_argument('--result', choices=['slot_found', 'no_common_slot', 'error', 'invalid_response'], default='slot_found')
parser.add_argument('--signed-in', action='store_true')
parser.add_argument('--connected', action='store_true')
parser.add_argument('--pending-once', action='store_true')
parser.add_argument('--booking-result', choices=['booked','confirmation_required','slot_unavailable','unknown','failed','missing'], default='booked')
args = parser.parse_args()
origin = f'http://127.0.0.1:{args.port}'
root = Path(__file__).resolve().parents[1]
seen = set()
bookings = {}
tasks = {}

class Handler(BaseHTTPRequestHandler):
    def reply(self, value, status=200):
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_GET(self):
        if self.path == '/auth/me':
            self.reply({'id':'test-account', 'name':'Test Person', 'email':'test@example.com','calendar_connected':args.connected} if args.signed_in else {'error':'sign_in_required'}, 200 if args.signed_in else 401)
            return
        if self.path.startswith('/calendar/tasks/'):
            task = tasks.get(self.path.rsplit('/',1)[-1])
            if task is None:
                self.reply({'error':'unknown_task'},404)
            else:
                task['polls'] += 1
                status = 'working' if task['polls'] < (4 if args.pending_once else 2) else ('no_common_slot' if args.result == 'no_common_slot' else 'failed' if args.result == 'error' else 'needs_attention' if args.booking_result == 'unknown' else 'booked')
                self.reply({'id':task['id'],'status':status,'result':{'status':status,'reserved':status=='booked','slot':{'start':'2030-01-15T09:30:00Z','end':'2030-01-15T10:15:00Z'},'explanation':'Selected from both calendars using evidenced preferences and shared availability.','previous_meetings':[{'title':'Project catch-up','start':'2026-09-10T09:30:00Z'}]}})
            return
        if self.path.startswith('/bookings/'):
            op = bookings.get(self.path.split('/')[-1])
            if op is None:
                self.reply({'error':'unknown_booking'},404)
            else:
                status = 'booked' if args.booking_result == 'missing' else args.booking_result
                self.reply({**op,'status':status,'reserved':status=='booked'})
            return
        if self.path.startswith('/agents/'):
            tenant = self.path.split('/')[2]
            if tenant not in ['test-host', 'test-guest']:
                self.reply({}, 404)
            elif self.path.endswith('/schedule'):
                self.reply({'id':tenant, 'mock':False, 'reserved':False, 'schedule':{'title':'A conversation together','duration_minutes':30,'timezone':'Europe/Paris'}})
            else:
                self.reply({'supportedInterfaces': [{'url': origin + '/a2a', 'protocolBinding': 'JSONRPC', 'protocolVersion': '1.0', 'tenant': tenant}]})
            return
        text = (root / 'web/index.html').read_text().replace("const API = 'https://api.calendar.aithos.world'", f"const API = '{origin}'").replace("const WEBSITE = 'https://calendar.aithos.world'", f"const WEBSITE = '{origin}'")
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.end_headers()
        self.wfile.write(text.encode())

    def do_POST(self):
        if self.path == '/auth/agent':
            self.reply({'id':'test-account','identifier':'urn:aithos:calendar:agent:test-account','publication_status':'published','calendar_connected':False,'share_url':origin+'/book/test-account','agent_card_url':'https://registry.aithos.world/v1/agents/test-account/agent-card.json'})
            return
        if self.path == '/auth/logout':
            args.signed_in=False
            self.send_response(204)
            self.end_headers()
            return
        value = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        time.sleep(.3)  # Make the disabled form/loading state observable.
        if self.path == '/calendar/tasks':
            task_id = 'gc' + 'a'*40
            tasks.setdefault(task_id,{'id':task_id,'polls':0})
            self.reply({'id':task_id,'status':'queued'},202)
        elif self.path == '/calendar/proposals':
            if args.result == 'error':
                self.reply({'error':'host_calendar_not_connected'},409)
            else:
                self.reply({'status':args.result,'proposal':{'id':'gc'+'a'*40,'host':'test-host','peer':'test-account','slot':{'start':'2030-01-15T09:30:00Z','end':'2030-01-15T10:00:00Z'},'timezone':'Europe/Paris'},'reserved':False})
        elif self.path == '/calendar/bookings':
            self.reply({'id':value['id'],'status':args.booking_result,'slot':{'start':'2030-01-15T09:30:00Z','end':'2030-01-15T10:00:00Z'},'reserved':args.booking_result=='booked'})
        elif self.path == '/agents':
            url = value.get('booking_page_url', '')
            if not url.startswith('https://calendar.app.google/'):
                self.reply({'error': 'invalid_booking_page_url'}, 400)
                return
            tenant = 'test-host' if url.endswith('/host') else 'test-guest'
            pending = args.pending_once and tenant not in seen
            seen.add(tenant)
            self.reply({'id': tenant, 'identifier': 'urn:aithos:calendar:agent:' + tenant,
                        'share_url': origin + '/book/' + tenant, 'mock': False,
                        'publication_status': 'pending' if pending else 'published'}, 202 if pending else 200)
        elif self.path == '/bookings':
            if args.booking_result == 'missing' and not value.get('attendee', {}).get('email'):
                self.reply({'error':'attendee_details_required','missing_fields':['email']},422)
                return
            op = {k:value[k] for k in ['id','host','peer','slot']}
            bookings[op['id']] = op
            self.reply({**op,'status':'pending','reserved':False,'retry_after_ms':3000},202)
        elif self.path == '/a2a':
            params = value['params']
            request = params['message']['parts'][0]['data']
            data = {'duration_minutes':30, 'status': args.result, 'mock': False, 'reserved': False,
                    'organizer': 'urn:aithos:calendar:agent:' + params['tenant'],
                    'peer': request['peer'], 'trace_id': params['metadata']['calendarTraceId'],
                    'slot': {'start': '2030-01-15T09:30:00Z', 'end': '2030-01-15T10:00:00Z'} if args.result == 'slot_found' else None}
            if args.result == 'error': data['code'] = 'peer_unavailable'
            if args.result == 'invalid_response': data['reserved'] = True
            self.reply({'jsonrpc': '2.0', 'id': value['id'], 'result': {'message': {'parts': [{'data': data}]}}})
        else:
            self.reply({}, 404)

print(f'Isolated preview: {origin}, result={args.result}', flush=True)
ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()
