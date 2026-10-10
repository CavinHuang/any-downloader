// 基础库单元测试（node scripts/test_libs.js）
// 覆盖 extension/lib/zip.js（ZIP STORE 打包器）与 extension/lib/qr.js（QR 编码器）。
// 零依赖：zip 解不了就做结构断言（本地头/中央目录/EOCD 签名、CRC 已知向量、
// 原始数据回读、node zlib.crc32 交叉校验、系统 bsdtar 端到端解包）；QR 无现成
// 解码器，用结构断言（尺寸/定时线/定位图形/校正图形/格式信息双拷贝一致）+
// RS 校验子全零（GF(256) 独立复算）+ 数据层回读（反掩码 → 之字形 → 反交错 →
// 与输入逐字节比对）佐证可扫性。
"use strict";
const path = require("path");
const fs = require("fs");
const os = require("os");
const zlib = require("zlib");
const { execFileSync } = require("child_process");

globalThis.self = globalThis; // 两个库都挂在 self.* 命名空间

require(path.join("..", "extension", "lib", "zip.js"));
require(path.join("..", "extension", "lib", "qr.js"));

let pass = 0, fail = 0;
function ok(cond, label, extra) {
  if (cond) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + (extra !== undefined ? "  => " + JSON.stringify(extra) : "")); }
}
function eq(a, b, label) {
  if (a === b) { pass++; console.log("  ok  " + label); }
  else { fail++; console.log("  FAIL " + label + "\n    期望: " + JSON.stringify(b) + "\n    实际: " + JSON.stringify(a)); }
}

// ---------------------------------------------------------------- 通用工具
const utf8 = (s) => new TextEncoder().encode(s);
const bytesEq = (a, b) =>
  a.length === b.length && Array.from(a).every((v, i) => v === b[i]);
const rd16 = (b, o) => b[o] | (b[o + 1] << 8);
const rd32 = (b, o) =>
  (b[o] | (b[o + 1] << 8) | (b[o + 2] << 16) | (b[o + 3] << 24)) >>> 0;

// 确定性伪随机字节流（LCG），避免用 Math.random 破坏测试可重复性
function pseudoRandom(n) {
  const out = new Uint8Array(n);
  let s = 0x12345678;
  for (let i = 0; i < n; i++) {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    out[i] = s >>> 24;
  }
  return out;
}

const FIX_DATE = new Date(2026, 0, 1, 12, 30, 5);
// 2026-01-01 12:30:05 → DOS 时间/日期字段
const FIX_DOS_TIME = (12 << 11) | (30 << 5) | (5 >>> 1); // 25538
const FIX_DOS_DATE = ((2026 - 1980) << 9) | (1 << 5) | 1; // 23585

(async () => {
  // ================================================================ ADLZip
  console.log("== ADLZip（ZIP STORE 打包器）");

  eq(ADLZip.crc32(utf8("")), 0, "CRC32 空输入 = 0");
  eq(ADLZip.crc32(utf8("123456789")), 0xcbf43926, "CRC32 已知向量 '123456789' = 0xCBF43926");
  if (typeof zlib.crc32 === "function") {
    const rnd = pseudoRandom(65536);
    eq(ADLZip.crc32(rnd), zlib.crc32(rnd) >>> 0, "CRC32 与 node zlib.crc32 一致（64KB 伪随机）");
  } else {
    console.log("  skip CRC32 交叉校验（当前 node 无 zlib.crc32）");
  }

  // —— 构建一个有代表性的包：中文文件名 + 空文件 + 二进制 + 大文件
  const FILES = [
    { name: "article.md", data: utf8("# 标题\n\n正文：中文内容 mixed with English。\n") },
    { name: "images/图 片_001.png", data: pseudoRandom(256) },
    { name: "images/empty.bin", data: new Uint8Array(0) },
    { name: "big.dat", data: pseudoRandom(65536) },
  ];
  const pz = ADLZip.build(FILES, { date: FIX_DATE });
  ok(pz instanceof Promise, "build 返回 Promise");
  const z = await pz;
  ok(z instanceof Uint8Array, "build 产物为 Uint8Array");

  // —— EOCD：无注释时恰 22 字节，位于文件末尾
  const end = z.length;
  eq(rd32(z, end - 22), 0x06054b50, "EOCD 签名 0x06054b50 在结尾 -22 处");
  eq(rd16(z, end - 22 + 20), 0, "EOCD 注释长度 = 0");
  eq(rd16(z, end - 22 + 10), FILES.length, "EOCD 总条目数 = 4");
  const cdSize = rd32(z, end - 22 + 12);
  const cdOff = rd32(z, end - 22 + 16);
  eq(cdOff + cdSize, end - 22, "中央目录偏移+大小 紧贴 EOCD");

  // —— 逐条目核对中央目录与本地头
  let cdSig = true, localSig = true, offMatch = true, utf8Flag = true;
  let storeMethod = true, crcMatch = true, sizeMatch = true, nameMatch = true;
  let dataMatch = true, extLen0 = true, dosOk = true, emptyOk = true;
  let p = cdOff;
  const cdNames = [];
  for (let i = 0; i < FILES.length; i++) {
    if (rd32(z, p) !== 0x02014b50) cdSig = false;
    const flags = rd16(z, p + 8);
    const method = rd16(z, p + 10);
    const crc = rd32(z, p + 16);
    const csize = rd32(z, p + 20);
    const usize = rd32(z, p + 24);
    const nlen = rd16(z, p + 28);
    const loff = rd32(z, p + 42);
    const name = new TextDecoder().decode(z.subarray(p + 46, p + 46 + nlen));
    cdNames.push(name);
    if (!(flags & 0x0800)) utf8Flag = false;
    if (method !== 0) storeMethod = false;
    if (crc !== ADLZip.crc32(FILES[i].data)) crcMatch = false;
    if (csize !== FILES[i].data.length || usize !== FILES[i].data.length) sizeMatch = false;

    // 本地文件头
    if (rd32(z, loff) !== 0x04034b50) localSig = false;
    if (!(rd16(z, loff + 6) & 0x0800)) utf8Flag = false;
    if (rd16(z, loff + 8) !== 0) storeMethod = false;
    if (rd16(z, loff + 28) !== 0) extLen0 = false;
    if (rd16(z, loff + 10) !== FIX_DOS_TIME || rd16(z, loff + 12) !== FIX_DOS_DATE) dosOk = false;
    if (rd32(z, loff + 14) !== crc) crcMatch = false;
    const dlen = rd32(z, loff + 22);
    if (dlen !== FILES[i].data.length) sizeMatch = false;
    const data = z.subarray(loff + 30 + rd16(z, loff + 26), loff + 30 + rd16(z, loff + 26) + dlen);
    if (!bytesEq(data, FILES[i].data)) dataMatch = false; // STORE：读回原样即解压
    if (name !== FILES[i].name.replace(/\\/g, "/")) nameMatch = false;
    if (FILES[i].data.length === 0 && crc !== 0) emptyOk = false;

    // 本地头数据区结束应正好是下一本地头/中央目录的起点（无间隙错位）
    const expectNext = i + 1 < FILES.length ? null : cdOff;
    if (expectNext !== null && loff + 30 + rd16(z, loff + 26) + dlen !== expectNext) offMatch = false;
    p += 46 + nlen;
  }
  eq(p, cdOff + cdSize, "中央目录遍历字节数吻合");
  ok(cdSig, "中央目录条目签名 0x02014b50 ×4");
  ok(localSig, "本地文件头签名 0x04034b50 ×4");
  ok(utf8Flag, "UTF-8 文件名标志位 0x0800（本地头与中央目录均置位）");
  ok(storeMethod, "压缩方式 = 0（STORE）");
  ok(crcMatch, "CRC 字段与重算一致（本地头与中央目录）");
  ok(sizeMatch, "压缩后/未压缩大小字段一致（STORE 两值相等）");
  ok(nameMatch, "文件名 UTF-8 往返一致（含中文名）");
  eq(cdNames[1], "images/图 片_001.png", "中文文件名逐字节核对");
  ok(dataMatch, "数据区回读与输入逐字节一致（STORE 原样存储）");
  ok(extLen0, "扩展字段长度 = 0");
  ok(dosOk, "DOS 时间/日期字段符合固定日期");
  ok(emptyOk, "空文件条目 CRC = 0");
  ok(offMatch, "末条目数据区结束正好衔接中央目录（无错位）");

  // —— 确定性与边界
  const z2 = await ADLZip.build(FILES, { date: FIX_DATE });
  ok(bytesEq(z, z2), "同输入同日期两次构建字节级一致（确定性）");
  const zb = await ADLZip.build([{ name: "a\\b\\c.txt", data: "x" }], { date: FIX_DATE });
  eq(
    new TextDecoder().decode(zb.subarray(30, 30 + rd16(zb, 26))),
    "a/b/c.txt",
    "反斜杠路径归一为正斜杠"
  );
  const z0 = await ADLZip.build([], { date: FIX_DATE });
  eq(z0.length, 22, "空包 = 仅 22 字节 EOCD");
  eq(rd32(z0, 0), 0x06054b50, "空包 EOCD 签名");
  eq(rd16(z0, 8), 0, "空包条目数 = 0");
  if (typeof Blob === "function") {
    const blob = ADLZip.toBlob(z);
    ok(blob instanceof Blob && blob.type === "application/zip", "toBlob 返回 application/zip 的 Blob");
  } else {
    console.log("  skip toBlob（环境无全局 Blob）");
  }
  let threw = false;
  try { await ADLZip.build("nope"); } catch (_) { threw = true; }
  ok(threw, "非法入参（非数组）抛 TypeError");

  // —— 端到端解包：系统 bsdtar（Windows 内置 tar 即 libarchive）真实解压比对。
  //    仅用 ASCII 文件名参与比对（中文名的 tar 端转码属系统行为，结构断言已覆盖）。
  {
    let tarBin = null;
    const candidates =
      process.platform === "win32"
        ? [path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe"), "tar"]
        : ["tar"];
    for (const c of candidates) {
      try {
        const v = execFileSync(c, ["--version"], { stdio: ["ignore", "pipe", "ignore"] }).toString();
        if (v.includes("bsdtar")) { tarBin = c; break; }
      } catch (_) { /* 试下一个 */ }
    }
    if (!tarBin) {
      console.log("  skip 端到端解包（未找到支持 zip 的 bsdtar）");
    } else {
      const TAR_FILES = [
        { name: "article.md", data: utf8("# hello\n\nworld\n") },
        { name: "images/img_001.png", data: pseudoRandom(2048) },
        { name: "images/img_002.png", data: new Uint8Array(0) },
      ];
      const zipBytes = await ADLZip.build(TAR_FILES, { date: FIX_DATE });
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), "adlzip-test-"));
      try {
        const zipPath = path.join(dir, "sample.zip");
        fs.writeFileSync(zipPath, zipBytes);
        execFileSync(tarBin, ["-xf", zipPath, "-C", dir], { stdio: "ignore" });
        let allOk = true;
        for (const f of TAR_FILES) {
          const fp = path.join(dir, f.name.replace(/\//g, path.sep));
          if (!fs.existsSync(fp) || !bytesEq(fs.readFileSync(fp), f.data)) allOk = false;
        }
        ok(allOk, "bsdtar 端到端解包逐字节一致");
      } catch (e) {
        fail++;
        console.log("  FAIL bsdtar 端到端解包逐字节一致  => " + (e && e.message));
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    }
  }

  // ================================================================ ADLQR
  console.log("== ADLQR（QR 码编码器）");

  // —— 版本选择（byte 模式等级 M 容量：v1=14 字节 … v10=213 字节）
  const dEmpty = ADLQR.encodeDetail("");
  eq(dEmpty.version, 1, "空串 → 版本 1");
  eq(dEmpty.size, 21, "空串 → 尺寸 21");
  eq(ADLQR.encodeDetail("a".repeat(14)).version, 1, "14 字节 → 版本 1（v1 容量边界）");
  eq(ADLQR.encodeDetail("a".repeat(15)).version, 2, "15 字节 → 版本 2");
  const d10 = ADLQR.encodeDetail("a".repeat(213));
  eq(d10.version, 10, "213 字节 → 版本 10（v10 容量边界）");
  eq(d10.size, 57, "版本 10 → 尺寸 57");
  let threwLen = false;
  try { ADLQR.encodeDetail("a".repeat(214)); } catch (_) { threwLen = true; }
  ok(threwLen, "214 字节超出容量抛错");
  const dZh = ADLQR.encodeDetail("二维码测试");
  eq(dZh.version, 2, "中文按 UTF-8 字节计数（5 字 ×3B = 15B → 版本 2）");
  ok(
    [dEmpty, dZh, d10].every((d) => d.size === 17 + 4 * d.version),
    "矩阵尺寸符合版本公式 17 + 4×version"
  );
  ok(
    [dEmpty, dZh, d10].every((d) => Number.isInteger(d.mask) && d.mask >= 0 && d.mask <= 7),
    "掩码号 ∈ [0, 7]"
  );
  const detMats = [dEmpty, dZh, d10];
  ok(
    detMats.every((d) => d.matrix.every((row) => row.every((v) => typeof v === "boolean"))),
    "矩阵全部为 boolean（无空洞）"
  );

  // —— 定时线：第 6 行/第 6 列在 8..尺寸-9 区间明暗交替，端点为暗
  for (const d of [dZh, d10]) {
    let alt = true;
    for (let i = 8; i <= d.size - 9; i++) {
      if (d.matrix[6][i] !== (i % 2 === 0)) alt = false;
      if (d.matrix[i][6] !== (i % 2 === 0)) alt = false;
    }
    ok(alt, `定时线图案（版本 ${d.version}，第 6 行/列 8..${d.size - 9} 交替）`);
    ok(
      d.matrix[6][8] === true && d.matrix[8][6] === true,
      `定时线端点为暗（版本 ${d.version}）`
    );
  }

  // —— 定位图案：三个角各一个 7×7 同心环（外环暗 / 5×5 环亮 / 3×3 心暗）+ 分隔带全亮
  const checkFinders = (d, label) => {
    let shape = true, sep = true;
    for (const [r0, c0] of [[0, 0], [0, d.size - 7], [d.size - 7, 0]]) {
      for (let dr = 0; dr < 7; dr++)
        for (let dc = 0; dc < 7; dc++) {
          const ring = dr === 0 || dr === 6 || dc === 0 || dc === 6;
          const core = dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4;
          if (d.matrix[r0 + dr][c0 + dc] !== (ring || core)) shape = false;
        }
      // 分隔带：定位图形外一圈在矩阵内的格子全亮
      for (let i = -1; i <= 7; i++) {
        const rowLo = r0 + 7, rowHi = r0 - 1, colLo = c0 + 7, colHi = c0 - 1;
        if (rowLo < d.size && c0 + i >= 0 && c0 + i < d.size && d.matrix[rowLo][c0 + i] !== false) sep = false;
        if (rowHi >= 0 && c0 + i >= 0 && c0 + i < d.size && d.matrix[rowHi][c0 + i] !== false) sep = false;
        if (colLo < d.size && r0 + i >= 0 && r0 + i < d.size && d.matrix[r0 + i][colLo] !== false) sep = false;
        if (colHi >= 0 && r0 + i >= 0 && r0 + i < d.size && d.matrix[r0 + i][colHi] !== false) sep = false;
      }
    }
    ok(shape, label + "：三角色 7×7 定位图形形状");
    ok(sep, label + "：分隔带全亮");
  };
  checkFinders(dZh, "版本 2");
  checkFinders(d10, "版本 10");

  // —— 固定暗模块（尺寸-8, 8），规范要求恒为暗
  ok(dZh.matrix[dZh.size - 8][8] === true, "固定暗模块 (尺寸-8, 8) 为暗（版本 2）");

  // —— 校正图形（规范坐标表：v2=[6,18]，v10=[6,28,46]；与定位图形重叠的三个角跳过）
  const checkAlign = (d, r, c, label) => {
    let okShape = true;
    for (let dr = -2; dr <= 2; dr++)
      for (let dc = -2; dc <= 2; dc++) {
        const dist = Math.max(Math.abs(dr), Math.abs(dc));
        if (d.matrix[r + dr][c + dc] !== (dist !== 1)) okShape = false;
      }
    ok(okShape, label);
  };
  checkAlign(dZh, 18, 18, "校正图形（版本 2）位于 (18, 18)");
  for (const [r, c] of [[6, 28], [28, 6], [28, 28], [28, 50], [50, 28], [50, 50]])
    checkAlign(d10, r, c, `校正图形（版本 10）位于 (${r}, ${c})`);

  // —— 格式信息双拷贝一致（两份布局互为校验）
  const fmtBits1 = (d) => {
    const bits = [];
    for (let i = 0; i <= 5; i++) bits.push(d.matrix[i][8] ? 1 : 0);
    bits.push(d.matrix[7][8] ? 1 : 0);
    bits.push(d.matrix[8][8] ? 1 : 0);
    bits.push(d.matrix[8][7] ? 1 : 0);
    for (let i = 9; i < 15; i++) bits.push(d.matrix[8][14 - i] ? 1 : 0);
    return bits.join("");
  };
  const fmtBits2 = (d) => {
    const bits = [];
    for (let i = 0; i < 8; i++) bits.push(d.matrix[8][d.size - 1 - i] ? 1 : 0);
    for (let i = 8; i < 15; i++) bits.push(d.matrix[d.size - 15 + i][8] ? 1 : 0);
    return bits.join("");
  };
  for (const d of [dZh, d10])
    eq(fmtBits2(d), fmtBits1(d), `格式信息两份拷贝一致（版本 ${d.version}，M / 掩码 ${d.mask}）`);

  // —— 确定性
  const q1 = ADLQR.encode("https://example.com/post/123?from=qr");
  const q2 = ADLQR.encode("https://example.com/post/123?from=qr");
  ok(
    q1.length === q2.length && q1.every((row, i) => row.every((v, j) => v === q2[i][j])),
    "同输入两次 encode 输出确定"
  );
  ok(
    !q1.every((row) => row.every((v) => v === false)) &&
      !q1.every((row) => row.every((v) => v === true)),
    "输出非全亮/全暗（有内容）"
  );

  // —— 数据层回读 + RS 校验子：从矩阵反推码字流（独立复算，非调用库内部）
  //    复算件：GF(256) 表、分块参数表、校正图形坐标、功能图形布局、掩码公式
  {
    // GF(256)：本原多项式 0x11D
    const EXP = new Uint8Array(512);
    const LOG = new Uint8Array(256);
    for (let i = 0, x = 1; i < 255; i++) {
      EXP[i] = x;
      LOG[x] = i;
      x <<= 1;
      if (x & 0x100) x ^= 0x11d;
    }
    for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
    const gmul = (a, b) => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);

    // 规范分块参数（等级 M）：[总数据码字, 每块纠错码字, 块数, 总码字]
    const SPEC = {
      1: [16, 10, 1, 26], 2: [28, 16, 1, 44], 3: [44, 26, 1, 70],
      4: [64, 18, 2, 100], 5: [86, 24, 2, 134], 6: [108, 16, 4, 172],
      7: [124, 18, 4, 196], 8: [154, 22, 4, 242], 9: [182, 22, 5, 292],
      10: [216, 26, 5, 346],
    };
    // 规范校正图形中心坐标（末位 = 尺寸-7）
    const ALIGN = { 1: [], 2: [6, 18], 3: [6, 22], 4: [6, 26], 5: [6, 30], 6: [6, 34], 7: [6, 22, 38], 8: [6, 24, 42], 9: [6, 26, 46], 10: [6, 28, 50] };
    // 掩码公式（x=列, y=行）
    const MASKS = [
      (x, y) => (x + y) % 2 === 0,
      (x, y) => y % 2 === 0,
      (x, y) => x % 3 === 0,
      (x, y) => (x + y) % 3 === 0,
      (x, y) => (((x / 3) | 0) + ((y / 2) | 0)) % 2 === 0,
      (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
      (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
      (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
    ];
    // 功能图形布局（定时线/定位图形+分隔带/校正图形/版本信息/格式信息/固定暗模块）
    const funcMap = (ver) => {
      const size = 17 + ver * 4;
      const f = Array.from({ length: size }, () => new Array(size).fill(false));
      const mark = (r, c) => {
        if (r >= 0 && c >= 0 && r < size && c < size) f[r][c] = true;
      };
      for (let i = 0; i < size; i++) { mark(6, i); mark(i, 6); }
      for (const [cr, cc] of [[3, 3], [3, size - 4], [size - 4, 3]])
        for (let dr = -4; dr <= 4; dr++)
          for (let dc = -4; dc <= 4; dc++) mark(cr + dr, cc + dc);
      const pos = ALIGN[ver];
      const last = pos[pos.length - 1];
      for (const r of pos)
        for (const c of pos) {
          if ((r === 6 && c === 6) || (r === 6 && c === last) || (r === last && c === 6)) continue;
          for (let dr = -2; dr <= 2; dr++)
            for (let dc = -2; dc <= 2; dc++) mark(r + dr, c + dc);
        }
      if (ver >= 7)
        for (let i = 0; i < 18; i++) {
          mark(size - 11 + (i % 3), (i / 3) | 0);
          mark((i / 3) | 0, size - 11 + (i % 3));
        }
      for (let i = 0; i <= 5; i++) mark(i, 8);
      for (let i = 0; i < 8; i++) mark(8, size - 1 - i); // 第二份上半段
      mark(7, 8); mark(8, 8); mark(8, 7);
      for (let i = 9; i < 15; i++) mark(8, 14 - i);
      for (let i = 8; i < 15; i++) mark(size - 15 + i, 8);
      mark(size - 8, 8);
      return f;
    };
    // 之字形读出非功能区模块 → 反掩码 → 收成码字节
    const readCodewords = (d) => {
      const f = funcMap(d.version);
      const isMask = MASKS[d.mask];
      const bits = [];
      for (let right = d.size - 1; right >= 1; right -= 2) {
        if (right === 6) right = 5;
        for (let vert = 0; vert < d.size; vert++) {
          for (let j = 0; j < 2; j++) {
            const c = right - j;
            const upward = ((right + 1) & 2) === 0;
            const r = upward ? d.size - 1 - vert : vert;
            if (!f[r][c]) {
              const v = d.matrix[r][c] ? 1 : 0;
              bits.push(isMask(c, r) ? v ^ 1 : v);
            }
          }
        }
      }
      const total = SPEC[d.version][3];
      const out = [];
      for (let i = 0; i < total; i++) {
        let b = 0;
        for (let k = 0; k < 8; k++) b = (b << 1) | bits[i * 8 + k];
        out.push(b);
      }
      return out;
    };
    // 反交错：交错流是「列优先（数据列再纠错列），长块索引在前」，
    // 逆操作 = 按同样顺序把字节逐个发还给各块（cw 已是交错后的流，不能线性切分）
    const deInterleave = (cw, ver) => {
      const [dataN, ecN, blocks] = SPEC[ver];
      const shortLen = Math.floor(dataN / blocks);
      const numLong = dataN % blocks;
      const lenOf = (b) => shortLen + (b < numLong ? 1 : 0);
      const blks = Array.from({ length: blocks }, () => new Array(shortLen + 1).fill(0));
      const ecs = Array.from({ length: blocks }, () => new Array(ecN).fill(0));
      let p2 = 0;
      for (let i = 0; i < shortLen + 1; i++)
        for (let b = 0; b < blocks; b++) if (i < lenOf(b)) blks[b][i] = cw[p2++];
      for (let i = 0; i < ecN; i++)
        for (let b = 0; b < blocks; b++) ecs[b][i] = cw[p2++];
      if (p2 !== SPEC[ver][3]) return null;
      // 短块申请了 shortLen+1 的槽位，修剪掉尾部富余，保证「数据‖纠错」拼接无空洞
      for (let b = 0; b < blocks; b++) blks[b] = blks[b].slice(0, lenOf(b));
      return { blks, ecs };
    };
    // RS 校验子：块码字多项式在 α^0..α^(ec-1) 处取值须全为 0
    const syndromesOk = (cw, ver) => {
      const parts = deInterleave(cw, ver);
      if (!parts) return false;
      const ecN = SPEC[ver][1];
      return parts.blks.every((blk, bi) => {
        const full = blk.concat(parts.ecs[bi]);
        for (let j = 0; j < ecN; j++) {
          let s = 0;
          for (const byte of full) s = gmul(s, EXP[j]) ^ byte; // Horner 求值 C(α^j)
          if (s !== 0) return false;
        }
        return true;
      });
    };
    // 码流头回读：先反交错，再按块序拼接全部数据码字（消息可跨块）
    const streamOk = (d, text) => {
      const parts = deInterleave(readCodewords(d), d.version);
      if (!parts) return "反交错字节数不吻合";
      const cw = [].concat(...parts.blks);
      const ccBits = d.version <= 9 ? 8 : 16;
      const bitAt = (i) => (cw[i >> 3] >> (7 - (i & 7))) & 1;
      let off = 0, mode = 0;
      for (let i = 0; i < 4; i++) mode = (mode << 1) | bitAt(off++);
      if (mode !== 0b0100) return "模式指示非 byte";
      let cc = 0;
      for (let i = 0; i < ccBits; i++) cc = (cc << 1) | bitAt(off++);
      if (cc !== utf8(text).length) return `计数 ${cc} ≠ 输入 ${utf8(text).length} 字节`;
      const got = new Uint8Array(cc);
      for (let i = 0; i < cc; i++) {
        let b = 0;
        for (let k = 0; k < 8; k++) b = (b << 1) | bitAt(off++);
        got[i] = b;
      }
      return bytesEq(got, utf8(text)) ? null : "数据字节与输入不一致";
    };

    const dRt1 = ADLQR.encodeDetail("二维码测试 https://example.com/文章");
    const err1 = streamOk(dRt1, "二维码测试 https://example.com/文章");
    eq(err1, null, "数据层回读：模式/计数/内容与输入一致（版本 " + dRt1.version + "）");
    ok(syndromesOk(readCodewords(dRt1), dRt1.version), `RS 校验子全零（版本 ${dRt1.version}，单块）`);

    const dRt2 = ADLQR.encodeDetail("a".repeat(213));
    eq(streamOk(dRt2, "a".repeat(213)), null, "数据层回读一致（版本 10，16 位计数）");
    ok(syndromesOk(readCodewords(dRt2), 10), "RS 校验子全零（版本 10，长短块混排 5 块）");

    const dRt3 = ADLQR.encodeDetail("a".repeat(80)); // 80B → 版本 5：2 块交错 + 7 余位模块
    eq(streamOk(dRt3, "a".repeat(80)), null, "数据层回读一致（版本 5，2 块交错 + 余位）");
    ok(syndromesOk(readCodewords(dRt3), dRt3.version), "RS 校验子全零（版本 5，2 块）");

    const dRt4 = ADLQR.encodeDetail("a".repeat(100)); // 100B → 版本 6：4 块交错
    eq(streamOk(dRt4, "a".repeat(100)), null, "数据层回读一致（版本 6，4 块交错）");
    ok(syndromesOk(readCodewords(dRt4), 6), "RS 校验子全零（版本 6，4 块）");
  }

  // ---------------------------------------------------------------- 汇总
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
})().catch((e) => {
  fail++;
  console.log("  FAIL 测试执行异常  => " + ((e && e.stack) || e));
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(1);
});
