// node markets/index/js/test-vectors.mjs  -> checks the JS codec against the golden vectors
import fs from 'node:fs';
import { encode, decode, sha256, verify, hexToBytes, bytesToHex, formatValue } from './oracle-print.mjs';
const here = new URL('.', import.meta.url);
const v = JSON.parse(fs.readFileSync(new URL('../vectors/oracle_print_v1.json', here)));
let pass = 0, fail = 0;
const check = (c, msg) => { if (c) pass++; else { fail++; console.error('FAIL', msg); } };
for (const x of v.vectors) {
  const bytes = hexToBytes(x.canonicalHex);
  const p = decode(bytes);
  check(bytesToHex(encode(p)) === x.canonicalHex, `${x.name} re-encode`);
  check(bytesToHex(await sha256(bytes)) === x.printHash, `${x.name} hash`);
  check(p.value.toString() === x.valueUnitsInt, `${x.name} value`);
  check(formatValue(p.value, p.valueScale) === x.value, `${x.name} formatted value`);
  check(p.sourceStartMs.toString() === x.sourceStartMs, `${x.name} start`);
  check(p.status === x.status && p.dataStatus === x.dataStatus && p.finality === x.finality, `${x.name} enums`);
  check(await verify(bytes, hexToBytes(x.pubkeyHex), hexToBytes(x.signatureHex)), `${x.name} signature`);
}
for (const x of v.negative) {
  const bytes = hexToBytes(x.canonicalHex);
  if (x.name === 'bit_flip_in_index_id') {
    decode(bytes);
    check(!(await verify(bytes, hexToBytes(x.pubkeyHex), hexToBytes(x.signatureHex))), x.name);
  } else {
    let err = '';
    try { decode(bytes); } catch (e) { err = e.message; }
    check(err === (x.name === 'trailing_byte' ? 'TrailingBytes' : 'BadEnum'), `${x.name} -> ${err}`);
  }
}
console.log(`js vectors: ${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
