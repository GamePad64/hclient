// HTTPS, but HTTP/1.1 only (ALPN offers no h2): isolates "needs HTTP/2"
// from "needs a secure context".
const https = require('node:https');
const fs = require('node:fs');
const PAGE = fs.readFileSync(__dirname + '/page.html');
const srv = https.createServer(
  {
    key: fs.readFileSync(__dirname + '/key.pem'),
    cert: fs.readFileSync(__dirname + '/cert.pem'),
    ALPNProtocols: ['http/1.1'],
  },
  (req, res) => {
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
        const buf = Buffer.concat(chunks.map((c) => c.data));
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            protocol: 'http/' + req.httpVersion,
            contentType: req.headers['content-type'] || null,
            bytesLen: buf.length,
            bytesUtf8: buf.toString('utf8'),
            chunks: chunks.map((c) => ({ t: c.t, len: c.data.length })),
          }),
        );
      });
      return;
    }
    if (url === '/report') {
      let b = '';
      req.on('data', (d) => (b += d));
      req.on('end', () => {
        fs.writeFileSync(
          __dirname + '/report-' + (req.headers['x-label'] || 'unknown') + '.json',
          b,
        );
        res.writeHead(204).end();
      });
      return;
    }
    res.writeHead(404).end();
  },
);
srv.listen(8803, '127.0.0.1', () => console.log('https-h1 on 8803'));
