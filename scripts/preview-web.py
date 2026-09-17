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
parser.add_argument('--pending-once', action='store_true')
parser.add_argument('--booking-result', choices=['booked','unknown','failed','missing'], default='booked')
args = parser.parse_args()
origin = f'http://127.0.0.1:{args.port}'
root = Path(__file__).resolve().parents[1]
seen = set()
bookings = {}

class Handler(BaseHTTPRequestHandler):
    def reply(self, value, status=200):
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_GET(self):
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
        value = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        time.sleep(.3)  # Make the disabled form/loading state observable.
        if self.path == '/agents':
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
