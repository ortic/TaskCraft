// A mail server for the browser tests: takes the e-mails TaskCraft sends over SMTP and lists them
// as JSON over HTTP (GET /messages: [{to, raw, text}]), decoded enough to find links in them.
//   node mail-sink.mjs     (E2E_SMTP_PORT, default 8796; E2E_MAIL_PORT, default 8797)
import http from 'node:http'
import net from 'node:net'

const smtpPort = Number(process.env.E2E_SMTP_PORT ?? 8796)
const httpPort = Number(process.env.E2E_MAIL_PORT ?? 8797)
const mails = []

/** The text of a message: each MIME part decoded by its transfer encoding. */
function decode(raw) {
  let text = ''
  for (const part of raw.split(/\r?\n--[^\r\n]+/)) {
    const at = part.search(/\r?\n\r?\n/)
    if (at < 0) continue
    const head = part.slice(0, at)
    const body = part.slice(at).trimStart()
    if (/content-transfer-encoding:\s*base64/i.test(head)) {
      text += Buffer.from(body.replace(/\s+/g, ''), 'base64').toString('utf8')
    } else if (/content-transfer-encoding:\s*quoted-printable/i.test(head)) {
      const bytes = body.replace(/=\r?\n/g, '').replace(/=([0-9A-F]{2})/gi, (_, h) => String.fromCharCode(parseInt(h, 16)))
      text += Buffer.from(bytes, 'latin1').toString('utf8')
    } else {
      text += body
    }
    text += '\n'
  }
  return text
}

net
  .createServer((sock) => {
    let buf = ''
    let data = null
    let to = []
    sock.setEncoding('latin1')
    sock.on('error', () => {})
    sock.write('220 sink ready\r\n')
    sock.on('data', (chunk) => {
      buf += chunk
      let i
      while ((i = buf.indexOf('\r\n')) >= 0) {
        const line = buf.slice(0, i)
        buf = buf.slice(i + 2)
        if (data !== null) {
          if (line === '.') {
            const raw = Buffer.from(data, 'latin1').toString('utf8')
            mails.push({ to, raw, text: decode(raw), at: new Date().toISOString() })
            data = null
            to = []
            sock.write('250 queued\r\n')
          } else {
            data += (line.startsWith('.') ? line.slice(1) : line) + '\r\n'
          }
          continue
        }
        const cmd = line.toUpperCase()
        if (cmd.startsWith('EHLO')) sock.write('250-sink\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n')
        else if (cmd.startsWith('RCPT TO:')) {
          const m = /<([^>]*)>/.exec(line)
          to.push((m ? m[1] : line.slice(8)).trim().toLowerCase())
          sock.write('250 ok\r\n')
        } else if (cmd === 'DATA') {
          data = ''
          sock.write('354 go ahead\r\n')
        } else if (cmd === 'QUIT') sock.end('221 bye\r\n')
        else sock.write('250 ok\r\n')
      }
    })
  })
  .listen(smtpPort, '127.0.0.1')

http
  .createServer((req, res) => {
    if (req.url === '/messages') {
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end(JSON.stringify(mails))
    } else if (req.url === '/health') {
      res.end('ok')
    } else {
      res.writeHead(404)
      res.end()
    }
  })
  .listen(httpPort, '127.0.0.1')

console.log(`mail sink: SMTP on ${smtpPort}, messages on http://127.0.0.1:${httpPort}/messages`)
