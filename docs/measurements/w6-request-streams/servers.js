// Two servers, one page, one byte-level observer.
//
// The observer is the SERVER: for every /upload it records each data event
// with a timestamp and concatenates the bytes, so we can say what actually
// arrived on the wire and when — not merely whether fetch() rejected.
const http = require('node:http');
const http2 = require('node:http2');
const fs = require('node:fs');

const PAGE = fs.readFileSync(__dirname + '/page.html');
const KEY = fs.readFileSync(__dirname + '/key.pem');
const CERT = fs.readFileSync(__dirname + '/cert.pem');

const reports = {};

function record(label, req, bodyChunks, headers) {
  const buf = Buffer.concat(bodyChunks.map((c) => c.data));
  return {
    protocol: label,
    contentType: headers['content-type'] || null,
    contentLength: headers['content-length'] || null,
    transferEncoding: headers['transfer-encoding'] || null,
    bytesLen: buf.length,
    bytesUtf8: buf.toString('utf8'),
    bytesHex: buf.toString('hex'),
    chunks: bodyChunks.map((c) => ({ t: c.t, len: c.data.length })),
  };
}

// ---------------- HTTP/1.1, plaintext ----------------
const h1 = http.createServer((req, res) => {
  const url = req.url.split('?')[0];
  if (url === '/page.html') {
    res.writeHead(200, { 'content-type': 'text/html' });
    return res.end(PAGE);
  }
  if (url === '/upload') {
    const t0 = Date.now();
    const chunks = [];
    req.on('data', (d) => chunks.push({ t: Date.now() - t0, data: d }));
    req.on('end', () => {
      const rec = record('http/' + req.httpVersion, req, chunks, req.headers);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(rec));
    });
    req.on('aborted', () => {});
    return;
  }
  if (url === '/report') {
    let b = '';
    req.on('data', (d) => (b += d));
    req.on('end', () => {
      reports[req.headers['x-label'] || 'unknown'] = b;
      fs.writeFileSync(
        __dirname + '/report-' + (req.headers['x-label'] || 'unknown') + '.json',
        b,
      );
      res.writeHead(204).end();
    });
    return;
  }
  res.writeHead(404).end();
});

// ---------------- HTTP/2 over TLS ----------------
const h2 = http2.createSecureServer({ key: KEY, cert: CERT, allowHTTP1: true });
h2.on('request', (req, res) => {
  const url = (req.url || '').split('?')[0];
  if (url === '/page.html') {
    res.writeHead(200, { 'content-type': 'text/html' });
    return res.end(PAGE);
  }
  if (url === '/upload') {
    const t0 = Date.now();
    const chunks = [];
    req.on('data', (d) => chunks.push({ t: Date.now() - t0, data: d }));
    req.on('end', () => {
      const label = req.httpVersion === '2.0' ? 'h2' : 'http/' + req.httpVersion;
      const rec = record(label, req, chunks, req.headers);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(rec));
    });
    req.on('aborted', () => {});
    return;
  }
  if (url === '/report') {
    let b = '';
    req.on('data', (d) => (b += d));
    req.on('end', () => {
      reports[req.headers['x-label'] || 'unknown'] = b;
      fs.writeFileSync(
        __dirname + '/report-' + (req.headers['x-label'] || 'unknown') + '.json',
        b,
      );
      res.writeHead(204).end();
    });
    return;
  }
  res.writeHead(404).end();
});

h1.listen(8801, '127.0.0.1', () => console.log('h1 on 8801'));
h2.listen(8802, '127.0.0.1', () => console.log('h2 on 8802'));
