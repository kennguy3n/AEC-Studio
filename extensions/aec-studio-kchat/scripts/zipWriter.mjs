/**
 * Minimal ZIP (APPNOTE 6.3.x) writer used by the .kcz build script.
 *
 * Only the subset the extension archive needs is implemented:
 *   - Store + deflate compression methods (deflate for everything we
 *     ship; KChat Desktop's extension loader accepts both).
 *   - Local file headers + central directory.
 *   - CRC-32 over uncompressed bytes (computed via a small lookup
 *     table — no Buffer.from(buffer.crc32) helper exists on Node, so
 *     we roll our own).
 *
 * `buildKczZip(files, { deflateRawSync })` returns a single `Buffer`
 * holding the .kcz. The caller persists it (atomically).
 */
import { Buffer } from "node:buffer";

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = (c & 1) !== 0 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function dosTime(date) {
  // We use a fixed timestamp so the .kcz is byte-deterministic.
  // 1980-01-01 00:00:00 is the zero value of the DOS time/date pair.
  return { time: 0, date: 0x0021 };
}

export function buildKczZip(files, { deflateRawSync }) {
  if (!(files instanceof Map)) {
    throw new TypeError("buildKczZip requires a Map<string, Buffer>");
  }
  const chunks = [];
  const central = [];
  let offset = 0;

  const sortedNames = [...files.keys()].sort();
  for (const name of sortedNames) {
    const data = files.get(name);
    const utf8Name = Buffer.from(name, "utf8");
    const crc = crc32(data);
    const compressed = deflateRawSync(data, { level: 9 });
    const useDeflate = compressed.length < data.length;
    const payload = useDeflate ? compressed : data;
    const method = useDeflate ? 8 : 0;
    const { time, date } = dosTime();

    const localHeader = Buffer.alloc(30);
    localHeader.writeUInt32LE(0x04034b50, 0);
    localHeader.writeUInt16LE(20, 4); // version needed
    localHeader.writeUInt16LE(0x0800, 6); // flags: UTF-8 name
    localHeader.writeUInt16LE(method, 8);
    localHeader.writeUInt16LE(time, 10);
    localHeader.writeUInt16LE(date, 12);
    localHeader.writeUInt32LE(crc, 14);
    localHeader.writeUInt32LE(payload.length, 18);
    localHeader.writeUInt32LE(data.length, 22);
    localHeader.writeUInt16LE(utf8Name.length, 26);
    localHeader.writeUInt16LE(0, 28); // extra length

    chunks.push(localHeader, utf8Name, payload);

    const centralEntry = Buffer.alloc(46);
    centralEntry.writeUInt32LE(0x02014b50, 0);
    centralEntry.writeUInt16LE(20, 4); // version made by
    centralEntry.writeUInt16LE(20, 6); // version needed
    centralEntry.writeUInt16LE(0x0800, 8); // flags
    centralEntry.writeUInt16LE(method, 10);
    centralEntry.writeUInt16LE(time, 12);
    centralEntry.writeUInt16LE(date, 14);
    centralEntry.writeUInt32LE(crc, 16);
    centralEntry.writeUInt32LE(payload.length, 20);
    centralEntry.writeUInt32LE(data.length, 24);
    centralEntry.writeUInt16LE(utf8Name.length, 28);
    centralEntry.writeUInt16LE(0, 30); // extra
    centralEntry.writeUInt16LE(0, 32); // comment
    centralEntry.writeUInt16LE(0, 34); // disk number start
    centralEntry.writeUInt16LE(0, 36); // internal attrs
    centralEntry.writeUInt32LE(0, 38); // external attrs
    centralEntry.writeUInt32LE(offset, 42); // local header offset

    central.push(centralEntry, utf8Name);
    offset += localHeader.length + utf8Name.length + payload.length;
  }

  const centralStart = offset;
  let centralLength = 0;
  for (const c of central) centralLength += c.length;
  const endRecord = Buffer.alloc(22);
  endRecord.writeUInt32LE(0x06054b50, 0);
  endRecord.writeUInt16LE(0, 4); // disk
  endRecord.writeUInt16LE(0, 6); // start disk
  endRecord.writeUInt16LE(sortedNames.length, 8);
  endRecord.writeUInt16LE(sortedNames.length, 10);
  endRecord.writeUInt32LE(centralLength, 12);
  endRecord.writeUInt32LE(centralStart, 16);
  endRecord.writeUInt16LE(0, 20);

  return Buffer.concat([...chunks, ...central, endRecord]);
}
