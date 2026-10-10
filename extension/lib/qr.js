// 纯 JS QR 码编码器 —— 零依赖（extension/lib/qr.js）。
// 参考 code-box「生成海报」能力（utils/makerQRPost.ts，其用 npm qrcode 库画到
// canvas 再合成海报）；我们无构建链，故自实现海报二维码所需的最小编码器。
// 范围与取舍（按 ISO/IEC 18004，产物可直接被相机/微信扫出）：
//   - 仅 byte 模式（URL / 文本场景够用），纠错等级固定 M（可纠约 15% 码字）；
//   - 版本 1-10 自动选择（byte 容量 14~213 字节，矩阵 21~57 模块，海报够用；
//     注意 byte 模式计数位 v1-9 为 8 位、v10 起为 16 位）；
//   - RS 纠错在伽罗瓦域 GF(256)（本原多项式 0x11D）上做系统码余式，
//     分块参数含 v8/v9/v10 的长短块混排与码字交错；
//   - 掩码不固定 0，而是对 8 个候选按规则 1-4 打分选最优；规则 3 用
//     「1:1:3:1:1 回纹 + 一侧 4 亮」11 格滑窗近似规范的位置算法（重叠命中
//     计数略有出入，仅影响择掩码的相对分值，任一掩码产物均合规可扫）；
//     打分时格式区是掩码 0 的占位格式位（31 格，各候选间恒定，
//     对择掩码的相对比较影响可忽略），胜出后再重画为真实格式位；
//   - 输出 boolean[][] 模块矩阵（含定时线 / 定位图形 / 校正图形 / 格式与
//     版本信息全部功能图形）。调用方自行放大绘制：安静区 ≥ 4 模块留白。
// 挂 self.ADLQR；node 单测 scripts/test_libs.js 先置 globalThis.self 再 require。
(() => {
  "use strict";

  // —— GF(256)：EXP/LOG 对数表（本原多项式 x^8+x^4+x^3+x^2+1 = 0x11D）
  const EXP = new Uint8Array(512);
  const LOG = new Uint8Array(256);
  for (let i = 0, x = 1; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255]; // 倍周期展开免取模
  const gmul = (a, b) => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);

  // —— RS 生成多项式 ∏(x - α^i)（i = 0..deg-1；系数降幂、省略首项 1），按次数缓存
  const GEN = [];
  const genPoly = (deg) => {
    if (GEN[deg]) return GEN[deg];
    const g = new Uint8Array(deg);
    g[deg - 1] = 1; // 单项式 x^deg（去首项）
    for (let i = 0, root = 1; i < deg; i++, root = gmul(root, 2)) {
      // 乘 (x + α^i)：原地「移位相加」
      for (let j = 0; j < deg; j++) {
        g[j] = gmul(g[j], root);
        if (j + 1 < deg) g[j] ^= g[j + 1];
      }
    }
    return (GEN[deg] = g);
  };

  // 数据码字 → 纠错码字（系统码：码字多项式除以生成多项式的余式）
  const rsRemainder = (data, deg) => {
    const g = genPoly(deg);
    const res = new Uint8Array(deg);
    for (let i = 0; i < data.length; i++) {
      const factor = data[i] ^ res[0];
      res.copyWithin(0, 1); // 余式整体降一次幂
      res[deg - 1] = 0;
      if (factor !== 0) for (let j = 0; j < deg; j++) res[j] ^= gmul(g[j], factor);
    }
    return res;
  };

  // —— 版本 1-10 参数（纠错等级 M；ISO/IEC 18004 表 13/14 摘录）
  //    data=总数据码字；ec=每块纠错码字；blocks=分块数。
  //    长块数 = data % blocks（长块在前，数据码字比短块多 1）；
  //    总码字 = data + blocks × ec。
  const VERSIONS = [
    null,
    { data: 16, ec: 10, blocks: 1 }, // v1  总 26
    { data: 28, ec: 16, blocks: 1 }, // v2  总 44
    { data: 44, ec: 26, blocks: 1 }, // v3  总 70
    { data: 64, ec: 18, blocks: 2 }, // v4  总 100（2×32）
    { data: 86, ec: 24, blocks: 2 }, // v5  总 134（2×43）
    { data: 108, ec: 16, blocks: 4 }, // v6  总 172（4×27）
    { data: 124, ec: 18, blocks: 4 }, // v7  总 196（4×31）
    { data: 154, ec: 22, blocks: 4 }, // v8  总 242（2×39 + 2×38）
    { data: 182, ec: 22, blocks: 5 }, // v9  总 292（2×37 + 3×36）
    { data: 216, ec: 26, blocks: 5 }, // v10 总 346（1×44 + 4×43）
  ];
  const MAX_VERSION = 10;
  const FORMAT_BITS_M = 0; // 纠错等级 M 的格式位（规范：L=1 M=0 Q=3 H=2）

  // 校正图形中心坐标（v1 无；v2-6 两点、v7-10 三点，首末恒为 6 与 尺寸-7）
  const alignPos = (ver) => {
    if (ver === 1) return [];
    const num = ((ver / 7) | 0) + 2;
    const step = (((ver * 4 + num * 2 + 1) / (num * 2 - 2)) | 0) * 2;
    const pos = new Array(num);
    pos[0] = 6;
    for (let i = num - 1, p = ver * 4 + 10; i >= 1; i--, p -= step) pos[i] = p;
    return pos;
  };

  // —— 码流装配：模式指示 + 计数 + 数据 + 终止符 + 补齐 + 填充码字，
  //    再分块（长块在前）→ 各块 RS 纠错 → 按列交错成最终码字流
  const makeCodewords = (bytes, ver) => {
    const spec = VERSIONS[ver];
    const ccBits = ver <= 9 ? 8 : 16; // byte 模式字符计数位宽
    const capBits = spec.data * 8;
    const bits = [];
    const push = (v, n) => {
      for (let i = n - 1; i >= 0; i--) bits.push((v >>> i) & 1);
    };
    push(0b0100, 4); // 模式指示：byte = 0100
    push(bytes.length, ccBits);
    for (let i = 0; i < bytes.length; i++) push(bytes[i], 8);
    for (let i = 0; i < 4 && bits.length < capBits; i++) bits.push(0); // 终止符（至多 4 位，可截断）
    while (bits.length % 8 !== 0) bits.push(0); // 补齐到字节边界
    const PAD = [0xec, 0x11]; // 规范填充码字交替
    for (let i = 0; bits.length + 8 <= capBits; i++) push(PAD[i % 2], 8);

    const dataCw = new Uint8Array(spec.data);
    for (let i = 0; i < spec.data; i++)
      for (let b = 0; b < 8; b++) dataCw[i] = (dataCw[i] << 1) | bits[i * 8 + b];

    // 分块 + RS + 交错
    const nBlocks = spec.blocks;
    const shortLen = Math.floor(spec.data / nBlocks);
    const numLong = spec.data % nBlocks; // 长块排在前
    const blocks = [];
    let pos = 0;
    for (let i = 0; i < nBlocks; i++) {
      const len = shortLen + (i < numLong ? 1 : 0);
      const data = dataCw.subarray(pos, pos + len);
      pos += len;
      blocks.push({ data, ec: rsRemainder(data, spec.ec) });
    }
    const out = new Uint8Array(spec.data + spec.ec * nBlocks);
    let o = 0;
    for (let i = 0; i < shortLen + 1; i++)
      for (const blk of blocks) if (i < blk.data.length) out[o++] = blk.data[i];
    for (let i = 0; i < spec.ec; i++)
      for (const blk of blocks) out[o++] = blk.ec[i];
    return out;
  };

  // —— 主流程：选版本 → 画功能图形 → 之字形填码字 → 择掩码 → 画格式信息
  const encodeDetail = (text) => {
    const bytes = new TextEncoder().encode(String(text == null ? "" : text));
    let ver = 0;
    for (let v = 1; v <= MAX_VERSION; v++) {
      const ccBits = v <= 9 ? 8 : 16;
      if (4 + ccBits + bytes.length * 8 <= VERSIONS[v].data * 8) {
        ver = v;
        break;
      }
    }
    if (!ver)
      throw new Error(
        `ADLQR: 内容过长（${bytes.length} 字节；等级 M 上限为版本 10 的 213 字节）`
      );

    const size = 17 + ver * 4;
    const codewords = makeCodewords(bytes, ver);

    // modules：0 亮 1 暗；isFunc：功能区（数据与掩码都不碰）
    const modules = new Uint8Array(size * size);
    const isFunc = new Uint8Array(size * size);
    const set = (r, c, dark, func) => {
      if (r < 0 || c < 0 || r >= size || c >= size) return; // 边缘越界即无该模块
      modules[r * size + c] = dark ? 1 : 0;
      if (func) isFunc[r * size + c] = 1;
    };
    const get = (r, c) => modules[r * size + c] === 1;

    // —— 功能图形 1：定时线（第 6 行/列，明暗交替；两端后被定位图形覆盖）
    for (let i = 0; i < size; i++) {
      set(6, i, i % 2 === 0, true);
      set(i, 6, i % 2 === 0, true);
    }
    // —— 功能图形 2：三个定位图形（左上/右上/左下）7×7 同心环 + 四周分隔带，
    //    dist（切比雪夫距离）≠2 且 ≠4 为暗：外环暗、5×5 环亮、3×3 心暗
    const drawFinder = (cr, cc) => {
      for (let dr = -4; dr <= 4; dr++)
        for (let dc = -4; dc <= 4; dc++) {
          const d = Math.max(Math.abs(dr), Math.abs(dc));
          set(cr + dr, cc + dc, d !== 2 && d !== 4, true);
        }
    };
    drawFinder(3, 3);
    drawFinder(3, size - 4);
    drawFinder(size - 4, 3);
    // —— 功能图形 3：校正图形 5×5（v≥2），跳过与定位图形重叠的三个角位
    if (ver >= 2) {
      const pos = alignPos(ver);
      const last = pos[pos.length - 1];
      for (const r of pos)
        for (const c of pos) {
          if ((r === 6 && c === 6) || (r === 6 && c === last) || (r === last && c === 6)) continue;
          for (let dr = -2; dr <= 2; dr++)
            for (let dc = -2; dc <= 2; dc++)
              set(r + dr, c + dc, Math.max(Math.abs(dr), Math.abs(dc)) !== 1, true);
        }
    }
    // —— 功能图形 4：版本信息（v≥7），18 位 BCH(18,6) 双份镜像于右上方块与左下方块
    if (ver >= 7) {
      let rem = ver;
      for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
      const vbits = ((ver << 12) | rem) & 0x3ffff;
      for (let i = 0; i < 18; i++) {
        const bit = ((vbits >>> i) & 1) === 1;
        set(size - 11 + (i % 3), (i / 3) | 0, bit, true);
        set((i / 3) | 0, size - 11 + (i % 3), bit, true);
      }
    }
    // —— 功能图形 5：格式信息先用占位掩码 0 画一遍，把两份格式区 +
    //    固定暗模块圈成功能区（占位位稍后按胜出掩码重画覆盖）
    const drawFormat = (m) => {
      const data = (FORMAT_BITS_M << 3) | m;
      let rem = data;
      for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
      const fbits = ((data << 10) | rem) ^ 0x5412;
      const bit = (i) => ((fbits >>> i) & 1) === 1;
      for (let i = 0; i <= 5; i++) set(i, 8, bit(i), true);
      set(7, 8, bit(6), true);
      set(8, 8, bit(7), true);
      set(8, 7, bit(8), true);
      for (let i = 9; i < 15; i++) set(8, 14 - i, bit(i), true);
      for (let i = 0; i < 8; i++) set(8, size - 1 - i, bit(i), true);
      for (let i = 8; i < 15; i++) set(size - 15 + i, 8, bit(i), true);
      set(size - 8, 8, true, true); // 规范要求的固定暗模块
    };
    drawFormat(0);

    // —— 之字形填码字：右下角起、两列一组蛇形（先上后下交替），整列对跳过第 6 列；
    //    剩余未用模块（v2-6 各有 7 个余位）保持为亮
    let bitIdx = 0;
    const totalBits = codewords.length * 8;
    for (let right = size - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let vert = 0; vert < size; vert++) {
        for (let j = 0; j < 2; j++) {
          const c = right - j;
          const upward = ((right + 1) & 2) === 0;
          const r = upward ? size - 1 - vert : vert;
          if (!isFunc[r * size + c] && bitIdx < totalBits) {
            modules[r * size + c] = (codewords[bitIdx >>> 3] >>> (7 - (bitIdx & 7))) & 1;
            bitIdx++;
          }
        }
      }
    }

    // —— 掩码：8 个候选（参数 (x, y) = (列, 行)）
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
    const applyMask = (m) => {
      const f = MASKS[m];
      for (let r = 0; r < size; r++)
        for (let c = 0; c < size; c++)
          if (!isFunc[r * size + c] && f(c, r)) modules[r * size + c] ^= 1;
    };

    // —— 打分（值越小越好，仅用于相对比较）：
    //    规则 1：行/列同色连跑 ≥5 → 3 + (长度-5)
    //    规则 2：2×2 同色块 → 每块 +3
    //    规则 3：回纹「1:1:3:1:1 + 一侧 4 亮」滑窗命中 → 每次 +40（近似，见文件头）
    //    规则 4：暗模块占比偏离 50%，每满 5% → +10（规范 floor 版本）
    const PATS = ["10111010000", "00001011101"]; // 回纹核心 + 4 亮（两朝向）
    const hit = (r, c, dr, dc, pat) => {
      for (let i = 0; i < pat.length; i++) {
        const rr = r + dr * i;
        const cc = c + dc * i;
        if (rr < 0 || cc < 0 || rr >= size || cc >= size) return false;
        if (modules[rr * size + cc] !== pat.charCodeAt(i) - 48) return false;
      }
      return true;
    };
    const score = () => {
      let s = 0;
      for (let r = 0; r < size; r++) {
        let run = 1;
        for (let c = 1; c <= size; c++) {
          if (c < size && modules[r * size + c] === modules[r * size + c - 1]) run++;
          else {
            if (run >= 5) s += 3 + (run - 5);
            run = 1;
          }
        }
      }
      for (let c = 0; c < size; c++) {
        let run = 1;
        for (let r = 1; r <= size; r++) {
          if (r < size && modules[r * size + c] === modules[(r - 1) * size + c]) run++;
          else {
            if (run >= 5) s += 3 + (run - 5);
            run = 1;
          }
        }
      }
      for (let r = 0; r < size - 1; r++)
        for (let c = 0; c < size - 1; c++) {
          const v = modules[r * size + c];
          if (
            modules[r * size + c + 1] === v &&
            modules[(r + 1) * size + c] === v &&
            modules[(r + 1) * size + c + 1] === v
          )
            s += 3;
        }
      let hits = 0;
      for (const pat of PATS)
        for (let r = 0; r < size; r++)
          for (let c = 0; c + pat.length <= size; c++) {
            if (hit(r, c, 0, 1, pat)) hits++;
            if (hit(r, c, 1, 0, pat)) hits++;
          }
      s += hits * 40;
      let dark = 0;
      for (let i = 0; i < modules.length; i++) dark += modules[i];
      const total = size * size;
      s += Math.floor((Math.abs((dark / total) * 100 - 50) / 5)) * 10;
      return s;
    };

    // 择掩码：XOR 两次可还原，无需重画；格式信息最后按胜出掩码绘制
    let bestMask = 0;
    let bestScore = Infinity;
    let bestModules = null;
    for (let m = 0; m < 8; m++) {
      applyMask(m);
      const sc = score();
      if (sc < bestScore) {
        bestScore = sc;
        bestMask = m;
        bestModules = modules.slice();
      }
      applyMask(m);
    }
    modules.set(bestModules);
    drawFormat(bestMask);

    const matrix = [];
    for (let r = 0; r < size; r++) {
      const row = new Array(size);
      for (let c = 0; c < size; c++) row[c] = modules[r * size + c] === 1;
      matrix.push(row);
    }
    return { size, version: ver, mask: bestMask, matrix };
  };

  const encode = (text) => encodeDetail(text).matrix;

  self.ADLQR = { encode, encodeDetail };
})();
