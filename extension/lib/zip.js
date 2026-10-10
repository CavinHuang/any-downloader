// ZIP 打包器（仅 STORE 存储模式）—— 零依赖（extension/lib/zip.js）。
// 参考 code-box「Markdown 压缩包下载」能力（sync/adapters/platforms/zip-download.ts，
// 其用 npm jszip）；我们无构建链，故自实现最小 ZIP 写入器：
//   [本地文件头 PK\x03\x04 + 文件数据]×n + 中央目录 PK\x01\x02 + EOCD PK\x05\x06。
// 格式要点：
//   - STORE（压缩方式 0）不压缩：png/jpg/webp 本身不可压，存储模式写入/读取代价最低；
//   - 文件名一律按 UTF-8 编码并置通用标志位 0x0800（bit11），中文文件名必需；
//   - 所有长度/偏移为 32 位小端字段：单文件与总包上限 4 GiB，超出抛 RangeError；
//   - 默认时间戳取打包时刻本地时间，可传 opts.date 固定（测试需要确定性产物）；
//   - 路径分隔符统一为 /（ZIP 规范），目录无需单独建条目（条目名含路径即可）。
// 挂 self.ADLZip：页面 / content script / MV3 background（importScripts）通用；
// node 单测先 globalThis.self = globalThis 再 require（scripts/test_libs.js）。
(() => {
  "use strict";

  // —— CRC-32（IEEE 802.3，反射多项式 0xEDB88320）：ZIP 头部要求的标准算法，
  //    已知向量 "123456789" → 0xCBF43926。查表 256 项，一次一字节。
  const CRC_TABLE = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    CRC_TABLE[n] = c >>> 0;
  }
  const crc32 = (u8) => {
    let c = 0xffffffff;
    for (let i = 0; i < u8.length; i++) c = CRC_TABLE[(c ^ u8[i]) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };

  // —— 小端写入助手（ZIP 所有字段均为小端）
  const w16 = (b, o, v) => {
    b[o] = v & 0xff;
    b[o + 1] = (v >>> 8) & 0xff;
  };
  const w32 = (b, o, v) => {
    b[o] = v & 0xff;
    b[o + 1] = (v >>> 8) & 0xff;
    b[o + 2] = (v >>> 16) & 0xff;
    b[o + 3] = (v >>> 24) & 0xff;
  };

  // —— DOS 日期时间（ZIP 头部格式）：[时间, 日期]
  //    时间 = 时<<11 | 分<<5 | 秒/2；日期 = (年-1980)<<9 | 月<<5 | 日
  const dosDateTime = (d) => [
    ((d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >>> 1)) & 0xffff,
    (((Math.max(d.getFullYear(), 1980) - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate()) & 0xffff,
  ];

  // 条目数据归一化：Uint8Array 原样；字符串按 UTF-8；ArrayBuffer 包一层
  const toBytes = (v, enc) => {
    if (typeof v === "string") return enc.encode(v);
    if (v instanceof Uint8Array) return v;
    if (v instanceof ArrayBuffer) return new Uint8Array(v);
    throw new TypeError("ADLZip.build: data 须为 Uint8Array / string / ArrayBuffer");
  };

  // 同步构建核心（单遍算出总长 → 一次性分配 → 顺序写四个区段）
  const buildSync = (files, opts) => {
    if (!Array.isArray(files)) throw new TypeError("ADLZip.build: files 须为数组 [{ name, data }]");
    const enc = new TextEncoder();
    const [dTime, dDate] = dosDateTime(
      (opts && opts.date) instanceof Date ? opts.date : new Date()
    );

    const entries = files.map((f) => {
      if (!f || typeof f.name !== "string" || !f.name)
        throw new TypeError("ADLZip.build: 每项须含非空 name 字符串");
      const nameBytes = enc.encode(f.name.replace(/\\/g, "/"));
      const data = toBytes(f.data, enc);
      if (nameBytes.length > 0xffff || data.length > 0xffffffff)
        throw new RangeError("ADLZip.build: 文件名或内容过长");
      return { nameBytes, data, crc: crc32(data), offset: 0 };
    });

    let bodySize = 0;
    let cdSize0 = 0;
    for (const e of entries) {
      bodySize += 30 + e.nameBytes.length + e.data.length; // 本地头 30 定长
      cdSize0 += 46 + e.nameBytes.length; // 中央目录条目 46 定长
    }
    const total = bodySize + cdSize0 + 22; // EOCD 无注释恒 22 字节
    if (total > 0xffffffff) throw new RangeError("ADLZip.build: 包体超出 4 GiB（32 位长度字段上限）");
    const out = new Uint8Array(total);

    // —— 区段 1：本地文件头 + 数据（STORE：压缩后大小 = 原大小，CRC 为原数据 CRC）
    let off = 0;
    for (const e of entries) {
      e.offset = off;
      w32(out, off, 0x04034b50); // 签名 PK\x03\x04
      w16(out, off + 4, 20); // 解压所需版本 2.0
      w16(out, off + 6, 0x0800); // 通用标志位：bit11 文件名为 UTF-8
      w16(out, off + 8, 0); // 压缩方式 0 = STORE
      w16(out, off + 10, dTime);
      w16(out, off + 12, dDate);
      w32(out, off + 14, e.crc);
      w32(out, off + 18, e.data.length); // 压缩后大小
      w32(out, off + 22, e.data.length); // 未压缩大小
      w16(out, off + 26, e.nameBytes.length);
      w16(out, off + 28, 0); // 扩展字段长度
      out.set(e.nameBytes, off + 30);
      off += 30 + e.nameBytes.length;
      out.set(e.data, off);
      off += e.data.length;
    }

    // —— 区段 2：中央目录（比本地头多外部属性 / 本地头偏移等元数据）
    const cdOffset = off;
    for (const e of entries) {
      w32(out, off, 0x02014b50); // 签名 PK\x01\x02
      w16(out, off + 4, 0x0314); // 打包器版本：高字节 3 = Unix，低字节 20 = 规范 2.0
      w16(out, off + 6, 20); // 解压所需版本
      w16(out, off + 8, 0x0800); // 与本地头一致的 UTF-8 标志
      w16(out, off + 10, 0); // STORE
      w16(out, off + 12, dTime);
      w16(out, off + 14, dDate);
      w32(out, off + 16, e.crc);
      w32(out, off + 20, e.data.length);
      w32(out, off + 24, e.data.length);
      w16(out, off + 28, e.nameBytes.length);
      w16(out, off + 30, 0); // 扩展字段
      w16(out, off + 32, 0); // 文件注释
      w16(out, off + 34, 0); // 起始盘号
      w16(out, off + 36, 0); // 内部属性
      w32(out, off + 38, (0o100644 << 16) >>> 0); // 外部属性：Unix 权限 rw-r--r--
      w32(out, off + 42, e.offset); // 本地头偏移
      out.set(e.nameBytes, off + 46);
      off += 46 + e.nameBytes.length;
    }
    const cdSize = off - cdOffset;

    // —— 区段 3：EOCD（无注释时恰 22 字节，位于文件末尾，解压器由此定位中央目录）
    w32(out, off, 0x06054b50); // 签名 PK\x05\x06
    w16(out, off + 4, 0); // 本盘号
    w16(out, off + 6, 0); // 中央目录起始盘
    w16(out, off + 8, entries.length); // 本盘条目数
    w16(out, off + 10, entries.length); // 总条目数
    w32(out, off + 12, cdSize); // 中央目录大小
    w32(out, off + 16, cdOffset); // 中央目录偏移
    w16(out, off + 20, 0); // 注释长度
    return out;
  };

  // 对外：异步返回（对齐 jszip generateAsync 的用法，调用方直接 await）
  const build = (files, opts) => new Promise((resolve) => resolve(buildSync(files, opts)));

  // Uint8Array → Blob（环境无 Blob 时返回 null，调用方降级为字节自行处理）
  const toBlob = (bytes, mime) =>
    typeof Blob === "function" ? new Blob([bytes], { type: mime || "application/zip" }) : null;

  self.ADLZip = { crc32, build, toBlob };
})();
