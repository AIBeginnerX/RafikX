//! 브라우저 스모크 게이트 (S4 보강) — HTML/JS 산출물의 런타임 오류를 실제 브라우저로 잡는다.
//! 기원: 2026-08-29 사용자 실측 — "슈퍼마리오 게임이 실행도 안 되는" 결과물.
//! game.js 의 잔재 변수(camTarget)가 첫 프레임에서 ReferenceError 를 냈지만,
//! node --check(구문만)·eslint(미설치)·내장 보안 스캐너(보안 전용) 어디에도 걸리지 않았다.
//! "사용자가 결함을 발견하는 순간 = 게이트 설계의 실패" 원칙에 따라 추가된 게이트다.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ERROR_MARKER: &str = "__RAFIKX_BROWSER_ERROR__";
const GAME_CONTRACT_META: &str = "rafikx-browser-game-contract";
const PROBE_PATH: &str = "/__rafikx_probe.js";
const PROBE_RESULT_PATH: &str = "/__rafikx_probe_result/";
const PROBE_TOKEN_PLACEHOLDER: &str = "__RAFIKX_PROBE_TOKEN__";
const MAX_STAGED_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STAGED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STAGED_ENTRIES: usize = 25_000;
const MAX_STAGING_DURATION: Duration = Duration::from_secs(3);
const MAX_BROWSER_STDERR_BYTES: usize = 256 * 1024;
const MAX_BROWSER_ERRORS: usize = 64;
const MAX_BROWSER_ERROR_DETAIL_CHARS: usize = 300;
const MAX_DISCOVERY_ENTRIES: usize = 25_000;
const MAX_BROWSER_ENTRIES: usize = 8;
const MAX_DISCOVERY_DURATION: Duration = Duration::from_secs(3);
const MAX_DISCOVERY_HTML_BYTES: u64 = 256 * 1024;
const MAX_DISCOVERY_TOTAL_HTML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REFERENCE_GRAPH_ENTRIES: usize = 512;
const MAX_REFERENCE_GRAPH_BYTES: u64 = 16 * 1024 * 1024;
const SECURITY_HEADERS: &str = "Content-Security-Policy: default-src 'self'; base-uri 'none'; connect-src 'self'; font-src 'self' data:; form-action 'none'; frame-ancestors 'none'; frame-src 'none'; img-src 'self' data: blob:; media-src 'self' blob:; object-src 'none'; sandbox allow-same-origin allow-scripts; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:\r\nX-Content-Type-Options: nosniff\r\nX-DNS-Prefetch-Control: off\r\nReferrer-Policy: no-referrer\r\n";
const PROBE_SCRIPT: &str = r#"(() => {
  const nativeLog = console.log.bind(console);
  const nativeFetch = globalThis.fetch.bind(globalThis);
  const nativeAllSettled = Promise.allSettled.bind(Promise);
  const pendingErrors = [];
  const report = async (kind, value = '') => {
    const detail = value ? '/' + encodeURIComponent(String(value).slice(0, 300)) : '';
    const response = await nativeFetch('/__rafikx_probe_result/__RAFIKX_PROBE_TOKEN__/' + kind + detail, {
      method: 'POST', cache: 'no-store', credentials: 'omit', redirect: 'error'
    });
    if (!response.ok) throw new Error(`probe receipt failed: ${response.status}`);
  };
  const emit = (kind, value) => {
    const detail = kind + ': ' + String(value);
    nativeLog('__RAFIKX_BROWSER_ERROR__' + detail);
    pendingErrors.push(report('error', detail));
  };
  const originalError = console.error.bind(console);
  console.error = (...args) => {
    emit('console', args.map(value => String(value)).join(' '));
    originalError(...args);
  };
  window.addEventListener('error', event => {
    if (event.target === window) {
      emit('runtime', event.message || 'unknown runtime error');
    } else {
      emit('resource', event.target?.src || event.target?.href || event.target?.tagName || 'unknown resource');
    }
  }, true);
  window.addEventListener('unhandledrejection', event => emit('promise', event.reason || 'unhandled rejection'));
  const frames = count => new Promise(resolve => {
    const next = () => count-- <= 0 ? resolve() : requestAnimationFrame(next);
    requestAnimationFrame(next);
  });
  const frame = () => frames(2);
  const delay = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
  const gameState = api => String(api.state()).toLowerCase();
  const expectState = (api, expected) => {
    const actual = gameState(api);
    if (actual !== expected) throw new Error(`expected ${expected}, got ${actual}`);
  };
  const press = code => document.dispatchEvent(new KeyboardEvent('keydown', { code, bubbles: true }));
  const release = code => document.dispatchEvent(new KeyboardEvent('keyup', { code, bubbles: true }));
  const hash = value => {
    let result = 2166136261;
    const stride = Math.max(1, Math.floor(value.length / 50000));
    for (let index = 0; index < value.length; index += stride) {
      result ^= value.charCodeAt(index);
      result = Math.imul(result, 16777619);
    }
    return String(result >>> 0);
  };
  const MAX_CANVAS_OPERATIONS = 2048;
  const MAX_CANVAS_FRAME_BYTES = 64 * 1024;
  const MAX_CANVAS_OPERATION_BYTES = 512;
  const MAX_CANVAS_SAMPLES = 16;
  const MAX_CANVAS_DIMENSION = 8192;
  const MAX_CANVAS_PIXELS = 16 * 1024 * 1024;
  const MAX_PATH_COMMANDS = 512;
  const canvasTrackers = new WeakMap();
  const canvasKinds = new WeakMap();
  const pathTrackers = new WeakMap();
  const bitmapTrackers = new WeakMap();
  const probeCanvases = new WeakSet();
  const wrappedMethod = Symbol('rafikxCanvasWrapped');
  const nativePath2D = globalThis.Path2D;
  const native2DPrototype = globalThis.CanvasRenderingContext2D?.prototype;
  const nativeClearRect = native2DPrototype?.clearRect;
  const nativeFillRect = native2DPrototype?.fillRect;
  const nativeDrawImage = native2DPrototype?.drawImage;
  const nativeGetImageData = native2DPrototype?.getImageData;
  const normalizedNumber = value => Number.isFinite(Number(value))
    ? String(Math.round(Number(value) * 1000) / 1000)
    : String(value);
  const matrixEvidence = value => {
    if (value === undefined || value === null) return 'matrix:1:0:0:1:0:0';
    try {
      const component = (primary, alias, fallback) => {
        const candidate = value[primary] ?? value[alias] ?? fallback;
        return Number.isFinite(Number(candidate)) ? normalizedNumber(candidate) : null;
      };
      const components = [
        component('a', 'm11', 1), component('b', 'm12', 0),
        component('c', 'm21', 0), component('d', 'm22', 1),
        component('e', 'm41', 0), component('f', 'm42', 0),
      ];
      return components.some(component => component === null)
        ? null
        : `matrix:${components.join(':')}`;
    } catch (_) {
      return null;
    }
  };
  const mix = (seed, value) => {
    let result = seed >>> 0;
    for (let index = 0; index < value.length; index += 1) {
      result ^= value.charCodeAt(index);
      result = Math.imul(result, 16777619);
    }
    return result >>> 0;
  };
  const newPathTracker = () => ({
    digest: 2166136261, commands: 0, bytes: 0, drawable: false, hasPoint: false, trusted: true,
  });
  const clonePathTracker = source => ({ ...source });
  const pathCommand = (tracker, method, args) => {
    if (!tracker.trusted) return;
    const values = args.map(value => typeof value === 'number'
      ? normalizedNumber(value)
      : `${Object.prototype.toString.call(value)}:${hash(String(value))}`);
    const operation = [method, ...values].join('|');
    tracker.commands += 1;
    tracker.bytes += operation.length;
    if (tracker.commands > MAX_PATH_COMMANDS || operation.length > MAX_CANVAS_OPERATION_BYTES
        || tracker.bytes > MAX_CANVAS_FRAME_BYTES) {
      tracker.trusted = false;
      return;
    }
    tracker.digest = mix(tracker.digest, operation);
    if (method === 'moveTo') tracker.hasPoint = true;
    else if (method === 'lineTo' || method === 'bezierCurveTo'
        || method === 'quadraticCurveTo' || method === 'arcTo') {
      tracker.drawable ||= tracker.hasPoint;
      tracker.hasPoint = true;
    } else if (method === 'arc' && Number(args[2]) > 0) {
      tracker.drawable = true;
      tracker.hasPoint = true;
    } else if (method === 'ellipse' && Number(args[2]) > 0 && Number(args[3]) > 0) {
      tracker.drawable = true;
      tracker.hasPoint = true;
    } else if ((method === 'rect' || method === 'roundRect')
        && Number(args[2]) !== 0 && Number(args[3]) !== 0) {
      tracker.drawable = true;
      tracker.hasPoint = true;
    }
  };
  if (typeof nativePath2D === 'function') {
    const TrackedPath2D = function(...args) {
      const target = new.target === TrackedPath2D ? nativePath2D : new.target;
      const instance = Reflect.construct(nativePath2D, args, target);
      let tracker = newPathTracker();
      if (args[0] instanceof nativePath2D && pathTrackers.has(args[0])) {
        tracker = clonePathTracker(pathTrackers.get(args[0]));
      } else if (typeof args[0] === 'string') {
        const source = args[0];
        pathCommand(tracker, 'svg', [`${source.length}:${hash(source)}`]);
        tracker.drawable = source.length <= 50_000 && /[LlHhVvCcSsQqTtAa]/.test(source);
        tracker.trusted &&= source.length <= 50_000;
      }
      pathTrackers.set(instance, tracker);
      return instance;
    };
    Object.setPrototypeOf(TrackedPath2D, nativePath2D);
    TrackedPath2D.prototype = nativePath2D.prototype;
    globalThis.Path2D = TrackedPath2D;
    for (const method of [
      'addPath', 'closePath', 'moveTo', 'lineTo', 'bezierCurveTo',
      'quadraticCurveTo', 'arc', 'arcTo', 'ellipse', 'rect', 'roundRect',
    ]) {
      const original = nativePath2D.prototype[method];
      if (typeof original !== 'function') continue;
      nativePath2D.prototype[method] = function(...args) {
        const result = Reflect.apply(original, this, args);
        const tracker = pathTrackers.get(this) || newPathTracker();
        if (method === 'addPath' && args[0] instanceof nativePath2D) {
          const source = pathTrackers.get(args[0]);
          const transform = matrixEvidence(args[1]);
          if (!source?.trusted || transform === null) tracker.trusted = false;
          else {
            pathCommand(tracker, 'addPath', [source.digest, source.commands, transform]);
            tracker.drawable ||= source.drawable;
          }
        } else {
          pathCommand(tracker, method, args);
        }
        pathTrackers.set(this, tracker);
        return result;
      };
    }
  }
  const resetCanvasFrame = tracker => {
    tracker.frameOperations.clear();
    tracker.frameCalls = 0;
    tracker.frameBytes = 0;
  };
  const canvasTracker = context => {
    let tracker = canvasTrackers.get(context.canvas);
    if (!tracker) {
      tracker = {
        canvas: context.canvas,
        context,
        currentPath: newPathTracker(),
        frameOperations: new Set(),
        frameCalls: 0,
        frameBytes: 0,
        hasEligibleContent: false,
        lastContentDigest: '',
        observing: false,
        samples: [],
        trusted: true,
      };
      canvasTrackers.set(context.canvas, tracker);
    }
    return tracker;
  };
  const canvasFrameDigest = tracker => hash([...tracker.frameOperations].sort().join('\n'));
  const recordFrameOperation = (tracker, operation) => {
    if (!tracker.trusted) return;
    tracker.frameCalls += 1;
    tracker.frameBytes += operation.length;
    if (tracker.frameCalls > MAX_CANVAS_OPERATIONS
        || operation.length > MAX_CANVAS_OPERATION_BYTES
        || tracker.frameBytes > MAX_CANVAS_FRAME_BYTES) {
      if (tracker.observing) tracker.trusted = false;
      else resetCanvasFrame(tracker);
      return;
    }
    tracker.frameOperations.add(hash(operation));
    tracker.hasEligibleContent = true;
  };
  const imageDataEvidence = value => {
    if (typeof ImageData === 'undefined' || !(value instanceof ImageData)) return null;
    let digest = 2166136261;
    let visible = false;
    const pixels = value.data.length / 4;
    const stride = Math.max(1, Math.floor(pixels / 4096));
    for (let pixel = 0; pixel < pixels; pixel += stride) {
      const index = pixel * 4;
      const alpha = value.data[index + 3];
      digest = mix(digest, String(alpha));
      if (alpha !== 0) {
        visible = true;
        digest = mix(digest, `${value.data[index]}:${value.data[index + 1]}:${value.data[index + 2]}`);
      }
    }
    return { visible, value: `image-data:${value.width}x${value.height}:${digest}` };
  };
  const trackedCanvasEvidence = source => {
    const tracker = canvasTrackers.get(source);
    if (!tracker?.trusted || !tracker.hasEligibleContent) return null;
    if (!(source.width > 0 && source.height > 0)
        || source.width > MAX_CANVAS_DIMENSION || source.height > MAX_CANVAS_DIMENSION
        || source.width * source.height > MAX_CANVAS_PIXELS) {
      tracker.trusted = false;
      return null;
    }
    if (tracker.frameOperations.size > 0) {
      tracker.lastContentDigest = canvasFrameDigest(tracker);
      if (!tracker.observing) resetCanvasFrame(tracker);
    }
    if (!tracker.lastContentDigest) return null;
    return `canvas:${source.width}x${source.height}:${tracker.lastContentDigest}`;
  };
  const canvasArgument = value => {
    if (typeof value === 'number') return normalizedNumber(value);
    if (typeof value === 'string') return `string:${value.length}:${hash(value)}`;
    if (typeof HTMLImageElement !== 'undefined' && value instanceof HTMLImageElement) {
      const source = String(value.currentSrc || value.src || '');
      return `image:${value.naturalWidth}x${value.naturalHeight}:${source.length}:${hash(source)}`;
    }
    if ((typeof HTMLCanvasElement !== 'undefined' && value instanceof HTMLCanvasElement)
        || (typeof OffscreenCanvas !== 'undefined' && value instanceof OffscreenCanvas)) {
      return trackedCanvasEvidence(value);
    }
    if (typeof ImageBitmap !== 'undefined' && value instanceof ImageBitmap) {
      const tracked = bitmapTrackers.get(value);
      return tracked?.trusted && tracked.eligible
        ? `bitmap:${value.width}x${value.height}:${tracked.digest}`
        : null;
    }
    const imageData = imageDataEvidence(value);
    if (imageData) return imageData.visible ? imageData.value : null;
    const rendered = String(value);
    return `${Object.prototype.toString.call(value)}:${rendered.length}:${hash(rendered)}`;
  };
  let colorProbeContext;
  const paintMayBeVisible = style => {
    if (typeof style !== 'string') return false;
    try {
      if (!colorProbeContext) {
        const canvas = document.createElement('canvas');
        probeCanvases.add(canvas);
        canvas.width = 1;
        canvas.height = 1;
        colorProbeContext = canvas.getContext('2d', { willReadFrequently: true });
      }
      Reflect.apply(nativeClearRect, colorProbeContext, [0, 0, 1, 1]);
      colorProbeContext.globalAlpha = 1;
      colorProbeContext.globalCompositeOperation = 'source-over';
      colorProbeContext.fillStyle = style;
      Reflect.apply(nativeFillRect, colorProbeContext, [0, 0, 1, 1]);
      return Reflect.apply(nativeGetImageData, colorProbeContext, [0, 0, 1, 1]).data[3] !== 0;
    } catch (_) {
      return false;
    }
  };
  const transformEvidence = context => {
    const value = typeof context.getTransform === 'function' ? context.getTransform() : null;
    return value
      ? [value.a, value.b, value.c, value.d, value.e, value.f].map(normalizedNumber).join(':')
      : '';
  };
  const pathEvidence = (tracker, method, args) => {
    const candidate = typeof nativePath2D === 'function' && args[0] instanceof nativePath2D
      ? pathTrackers.get(args[0])
      : tracker.currentPath;
    if (!candidate?.trusted || !candidate.drawable) return null;
    return `path:${candidate.digest}:${candidate.commands}:${method}`;
  };
  const recordCanvasOperation = (context, method, args) => {
    if (probeCanvases.has(context.canvas)) return;
    const tracker = canvasTracker(context);
    if (method === 'beginPath') {
      tracker.currentPath = newPathTracker();
      return;
    }
    if (method === 'clearRect') {
      if (Number(args[0]) <= 0 && Number(args[1]) <= 0
          && Number(args[2]) >= context.canvas.width
          && Number(args[3]) >= context.canvas.height) {
        resetCanvasFrame(tracker);
        tracker.hasEligibleContent = false;
        tracker.lastContentDigest = '';
      }
      return;
    }
    if ([
      'closePath', 'moveTo', 'lineTo', 'bezierCurveTo', 'quadraticCurveTo',
      'arc', 'arcTo', 'ellipse', 'rect', 'roundRect',
    ].includes(method)) {
      pathCommand(tracker.currentPath, method, args);
      return;
    }
    let evidenceArgs = args.map(canvasArgument);
    if (method === 'putImageData') {
      const imageData = imageDataEvidence(args[0]);
      if (!imageData?.visible) return;
      evidenceArgs[0] = imageData.value;
    } else {
      if (Number(context.globalAlpha) <= 0) return;
      if (method === 'fillRect' || method === 'strokeRect') {
        if (Number(args[2]) === 0 || Number(args[3]) === 0) return;
      }
      if (method === 'fill' || method === 'stroke') {
        const path = pathEvidence(tracker, method, args);
        if (!path) return;
        const pathArgument = typeof nativePath2D === 'function' && args[0] instanceof nativePath2D;
        evidenceArgs = [path, ...(pathArgument ? args.slice(1) : args).map(canvasArgument)];
      }
      const paint = method === 'stroke' || method === 'strokeRect'
        ? context.strokeStyle
        : context.fillStyle;
      if ((method === 'fill' || method === 'fillRect'
          || method === 'stroke' || method === 'strokeRect')
          && !paintMayBeVisible(paint)) return;
      if (method === 'drawImage') {
        if (evidenceArgs[0] === null) return;
        const width = args.length >= 9 ? Number(args[7])
          : args.length >= 5 ? Number(args[3]) : Number(args[0]?.width);
        const height = args.length >= 9 ? Number(args[8])
          : args.length >= 5 ? Number(args[4]) : Number(args[0]?.height);
        if (width === 0 || height === 0) return;
      }
    }
    if (evidenceArgs.some(value => value === null)) return;
    const operation = [
      method,
      ...evidenceArgs,
      `fill:${hash(String(context.fillStyle))}`,
      `stroke:${hash(String(context.strokeStyle))}`,
      `alpha:${normalizedNumber(context.globalAlpha)}`,
      `composite:${context.globalCompositeOperation}`,
      `line:${normalizedNumber(context.lineWidth)}:${context.lineCap}:${context.lineJoin}`,
      `dash:${typeof context.getLineDash === 'function' ? context.getLineDash().map(normalizedNumber).join(',') : ''}`,
      `transform:${transformEvidence(context)}`,
    ].join('|');
    recordFrameOperation(tracker, operation);
  };
  const contextTypes = [globalThis.CanvasRenderingContext2D, globalThis.OffscreenCanvasRenderingContext2D]
    .filter((value, index, values) => typeof value === 'function' && values.indexOf(value) === index);
  const drawingMethods = [
    'clearRect', 'fillRect', 'strokeRect', 'drawImage', 'putImageData',
    'beginPath', 'closePath', 'moveTo', 'lineTo', 'bezierCurveTo',
    'quadraticCurveTo', 'arc', 'arcTo', 'ellipse', 'rect', 'roundRect',
    'fill', 'stroke',
  ];
  for (const Context of contextTypes) {
    for (const method of drawingMethods) {
      const original = Context.prototype[method];
      if (typeof original !== 'function' || original[wrappedMethod]) continue;
      const wrapped = function(...args) {
        const result = Reflect.apply(original, this, args);
        try { recordCanvasOperation(this, method, args); }
        catch (_) { canvasTracker(this).trusted = false; }
        return result;
      };
      wrapped[wrappedMethod] = true;
      Context.prototype[method] = wrapped;
    }
  }
  const wrapGetContext = Canvas => {
    if (typeof Canvas !== 'function') return;
    const original = Canvas.prototype.getContext;
    if (typeof original !== 'function' || original[wrappedMethod]) return;
    const wrapped = function(kind, ...args) {
      const context = Reflect.apply(original, this, [kind, ...args]);
      if (!probeCanvases.has(this) && context) {
        canvasKinds.set(this, String(kind).toLowerCase());
        if (String(kind).toLowerCase() === '2d') canvasTracker(context);
      }
      return context;
    };
    wrapped[wrappedMethod] = true;
    Canvas.prototype.getContext = wrapped;
  };
  wrapGetContext(globalThis.HTMLCanvasElement);
  wrapGetContext(globalThis.OffscreenCanvas);
  const imageBitmapArgumentEvidence = args => {
    if (args.length > 5) return null;
    try {
      const values = args.map(value => {
        if (typeof value === 'number') return `number:${normalizedNumber(value)}`;
        if (value === undefined) return 'undefined';
        if (value === null || typeof value !== 'object') return null;
        const keys = [
          'imageOrientation', 'premultiplyAlpha', 'colorSpaceConversion',
          'resizeWidth', 'resizeHeight', 'resizeQuality',
        ];
        return `options:${keys.map(key => {
          const candidate = value[key];
          return `${key}=${typeof candidate === 'number' ? normalizedNumber(candidate) : String(candidate ?? '')}`;
        }).join(',')}`;
      });
      const rendered = values.some(value => value === null) ? null : values.join('|');
      return rendered !== null && rendered.length <= MAX_CANVAS_OPERATION_BYTES ? rendered : null;
    } catch (_) {
      return null;
    }
  };
  if (typeof globalThis.createImageBitmap === 'function') {
    const original = globalThis.createImageBitmap;
    globalThis.createImageBitmap = async function(source, ...args) {
      const sourceDigest = ((typeof HTMLCanvasElement !== 'undefined' && source instanceof HTMLCanvasElement)
          || (typeof OffscreenCanvas !== 'undefined' && source instanceof OffscreenCanvas))
        ? trackedCanvasEvidence(source)
        : null;
      const argumentDigest = imageBitmapArgumentEvidence(args);
      const bitmap = await Reflect.apply(original, this, [source, ...args]);
      if (sourceDigest && argumentDigest !== null) {
        bitmapTrackers.set(bitmap, {
          trusted: true,
          eligible: true,
          digest: hash(`${sourceDigest}|createImageBitmap|${argumentDigest}`),
        });
      }
      return bitmap;
    };
  }
  if (typeof globalThis.OffscreenCanvas === 'function') {
    const original = globalThis.OffscreenCanvas.prototype.transferToImageBitmap;
    if (typeof original === 'function' && !original[wrappedMethod]) {
      const wrapped = function(...args) {
        const sourceDigest = trackedCanvasEvidence(this);
        const bitmap = Reflect.apply(original, this, args);
        if (sourceDigest) {
          bitmapTrackers.set(bitmap, {
            trusted: true,
            eligible: true,
            digest: hash(`${sourceDigest}|transferToImageBitmap`),
          });
        }
        const tracker = canvasTrackers.get(this);
        if (tracker) {
          resetCanvasFrame(tracker);
          tracker.hasEligibleContent = false;
          tracker.lastContentDigest = '';
          tracker.samples = [];
        }
        return bitmap;
      };
      wrapped[wrappedMethod] = true;
      globalThis.OffscreenCanvas.prototype.transferToImageBitmap = wrapped;
    }
  }
  const canvasVisualFingerprint = tracker => {
    const canvas = tracker.canvas;
    if (!(canvas.width > 0 && canvas.height > 0)
        || canvas.width > MAX_CANVAS_DIMENSION || canvas.height > MAX_CANVAS_DIMENSION
        || canvas.width * canvas.height > MAX_CANVAS_PIXELS) return null;
    try {
      if (!tracker.scratch) {
        tracker.scratch = document.createElement('canvas');
        probeCanvases.add(tracker.scratch);
        tracker.scratchContext = tracker.scratch.getContext('2d', { willReadFrequently: true });
      }
      const scale = Math.min(1, 128 / canvas.width, 128 / canvas.height);
      const width = Math.max(1, Math.round(canvas.width * scale));
      const height = Math.max(1, Math.round(canvas.height * scale));
      tracker.scratch.width = width;
      tracker.scratch.height = height;
      Reflect.apply(nativeDrawImage, tracker.scratchContext, [canvas, 0, 0, width, height]);
      const data = Reflect.apply(nativeGetImageData, tracker.scratchContext, [0, 0, width, height]).data;
      let digest = 2166136261;
      for (let index = 0; index < data.length; index += 4) {
        const alpha = data[index + 3];
        digest = mix(digest, String(alpha));
        if (alpha !== 0) digest = mix(digest, `${data[index]}:${data[index + 1]}:${data[index + 2]}`);
      }
      return String(digest);
    } catch (_) {
      return null;
    }
  };
  const sampleCanvasFrame = tracker => {
    if (!tracker.trusted || tracker.frameOperations.size === 0) {
      resetCanvasFrame(tracker);
      return;
    }
    const semantic = canvasFrameDigest(tracker);
    const visual = canvasVisualFingerprint(tracker);
    resetCanvasFrame(tracker);
    if (visual === null) {
      tracker.trusted = false;
      return;
    }
    tracker.samples.push({ semantic, visual });
    if (tracker.samples.length > MAX_CANVAS_SAMPLES) tracker.samples.shift();
  };
  const canvasGameplayFingerprint = canvas => {
    const tracker = canvasTrackers.get(canvas);
    if (!tracker?.observing || !tracker.trusted) return 'canvas2d:pending';
    for (let left = 0; left < tracker.samples.length; left += 1) {
      for (let right = left + 1; right < tracker.samples.length; right += 1) {
        if (tracker.samples[left].semantic !== tracker.samples[right].semantic
            && tracker.samples[left].visual !== tracker.samples[right].visual) {
          return `canvas2d-motion:${hash(tracker.samples.map(sample => `${sample.semantic}:${sample.visual}`).join('\n'))}`;
        }
      }
    }
    return 'canvas2d:pending';
  };
  const beginGameplayObservation = surface => {
    const canvases = [];
    for (const primary of surface.primaries) {
      if (primary instanceof HTMLCanvasElement) canvases.push(primary);
      for (const canvas of primary.querySelectorAll?.('canvas') || []) canvases.push(canvas);
    }
    const unique = [...new Set(canvases)];
    if (unique.length > 8) return;
    for (const canvas of unique) {
      const tracker = canvasTrackers.get(canvas);
      if (!tracker || canvasKinds.get(canvas) !== '2d') continue;
      tracker.observing = true;
      tracker.samples = [];
      tracker.trusted = true;
      resetCanvasFrame(tracker);
      const started = performance.now();
      let remaining = MAX_CANVAS_SAMPLES;
  const observe = () => {
        if (!tracker.observing || remaining <= 0 || performance.now() - started > 300) return;
        sampleCanvasFrame(tracker);
        remaining -= 1;
        requestAnimationFrame(observe);
      };
      requestAnimationFrame(observe);
    }
  };
  const visiblyPainted = element => {
    const rect = element.getBoundingClientRect();
    const left = Math.max(0, rect.left + 1);
    const right = Math.min(innerWidth - 1, rect.right - 1);
    const top = Math.max(0, rect.top + 1);
    const bottom = Math.min(innerHeight - 1, rect.bottom - 1);
    if (!(right >= left && bottom >= top)) return false;
    const points = [
      [(left + right) / 2, (top + bottom) / 2],
      [left, top], [right, top], [left, bottom], [right, bottom],
      [(left * 3 + right) / 4, (top * 3 + bottom) / 4],
      [(left + right * 3) / 4, (top * 3 + bottom) / 4],
      [(left * 3 + right) / 4, (top + bottom * 3) / 4],
      [(left + right * 3) / 4, (top + bottom * 3) / 4],
    ];
    return points.some(([x, y]) => {
      let current = element;
      let root = current.getRootNode();
      while (typeof ShadowRoot !== 'undefined' && root instanceof ShadowRoot) {
        const localTop = typeof root.elementFromPoint === 'function'
          ? root.elementFromPoint(x, y)
          : current;
        if (localTop !== current && !current.contains(localTop)) return false;
        current = root.host;
        root = current.getRootNode();
      }
      const documentTop = document.elementFromPoint(x, y);
      return documentTop === current || current.contains(documentTop);
    });
  };
  const visiblyRendered = element => {
    let current = element;
    while (current instanceof Element) {
      const style = getComputedStyle(current);
      if (style.display === 'none'
          || style.visibility === 'hidden'
          || style.visibility === 'collapse'
          || style.contentVisibility === 'hidden'
          || Number(style.opacity) <= 0) return false;
      if (current.parentElement) current = current.parentElement;
      else {
        const root = current.getRootNode();
        current = typeof ShadowRoot !== 'undefined' && root instanceof ShadowRoot
          ? root.host
          : null;
      }
    }
    const rect = element.getBoundingClientRect();
    return rect.width * rect.height >= 1
      && rect.bottom > 0
      && rect.right > 0
      && rect.top < innerHeight
      && rect.left < innerWidth
      && visiblyPainted(element);
  };
  const gameSurface = api => {
    if (typeof api.surface !== 'function') throw new Error('window.__rafikxGameTest.surface is missing');
    const candidate = api.surface();
    const root = typeof candidate === 'string' ? document.querySelector(candidate) : candidate;
    if (!(root instanceof Element)) throw new Error('game surface is not an Element or selector');
    const rootRect = root.getBoundingClientRect();
    if (!visiblyRendered(root) || rootRect.width * rootRect.height < 256) {
      throw new Error('game surface is not visibly rendered');
    }
    const selector = 'canvas,svg,[role="application"],#game,#arena,#board,#screen';
    const primaries = [];
    if (root.matches(selector)) primaries.push(root);
    for (const element of root.querySelectorAll(selector)) {
      if (!primaries.includes(element)) primaries.push(element);
    }
    const visiblePrimaries = primaries.filter(element => {
      const rect = element.getBoundingClientRect();
      return visiblyRendered(element) && rect.width * rect.height >= 256;
    });
    if (visiblePrimaries.length === 0) throw new Error('visible gameplay surface is missing');
    return { root, primaries: visiblePrimaries };
  };
  const renderedFingerprint = (root, includeText) => {
    const elements = [root, ...root.querySelectorAll('*')].slice(0, 512);
    const parts = [];
    if (includeText) parts.push(`innerText:${String(root.innerText || '').trim()}`);
    for (const element of elements) {
      if (!visiblyRendered(element)) continue;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      const before = getComputedStyle(element, '::before').content;
      const after = getComputedStyle(element, '::after').content;
      const isCanvas = element instanceof HTMLCanvasElement;
      const isImage = element instanceof HTMLImageElement;
      const isSvgShape = typeof SVGGraphicsElement !== 'undefined'
        && element instanceof SVGGraphicsElement
        && !['svg', 'text', 'tspan'].includes(element.localName);
      const containsText = String(element.textContent || '').trim().length > 0
        || !['none', 'normal', '""'].includes(before)
        || !['none', 'normal', '""'].includes(after);
      const borderWidth = ['borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth']
        .reduce((total, property) => total + (Number.parseFloat(style[property]) || 0), 0);
      const paintedBox = !['transparent', 'rgba(0, 0, 0, 0)'].includes(style.backgroundColor)
        || style.backgroundImage !== 'none'
        || borderWidth > 0
        || style.boxShadow !== 'none'
        || (Number.parseFloat(style.outlineWidth) || 0) > 0;
      if (!includeText && !(isCanvas || isImage || isSvgShape || (!containsText && paintedBox))) {
        continue;
      }
      const visual = [
        element.tagName,
        Math.round(rect.x * 10), Math.round(rect.y * 10),
        Math.round(rect.width * 10), Math.round(rect.height * 10),
        style.display, style.visibility, style.opacity, style.color,
        style.backgroundColor, style.backgroundImage, style.borderColor,
        style.transform, style.fill, style.stroke, style.font,
      ];
      if (includeText) {
        visual.push(
          before,
          after,
          Array.from(element.childNodes)
            .filter(node => node.nodeType === Node.TEXT_NODE)
            .map(node => String(node.textContent || '').trim())
            .filter(Boolean)
            .join(' '),
        );
      }
      parts.push(visual.join('|'));
      if (isCanvas) {
        if (includeText) {
          try { parts.push(`canvas:${hash(element.toDataURL())}`); }
          catch (error) { parts.push(`canvas-error:${String(error)}`); }
        } else {
          parts.push(canvasGameplayFingerprint(element));
        }
      } else if (isImage) {
        parts.push(`image:${element.currentSrc || element.src}`);
      } else if (isSvgShape) {
        try {
          const box = element.getBBox();
          parts.push(`svg:${box.x}:${box.y}:${box.width}:${box.height}`);
        } catch (_) {}
      }
    }
    return hash(parts.join('\n'));
  };
  const surfaceFingerprint = surface => renderedFingerprint(surface.root, true);
  const gameplayFingerprint = surface => hash(
    surface.primaries.map(element => renderedFingerprint(element, false)).join('\n')
  );
  const expectSurfaceChange = (before, after, transition) => {
    if (before === after) throw new Error(`game surface did not change for ${transition}`);
  };
  const runGameContract = async () => {
    if (!document.querySelector('meta[name="rafikx-browser-game-contract"][content="v1"]')) return;
    const api = window.__rafikxGameTest;
    if (!api || typeof api.state !== 'function' || typeof api.forceLoss !== 'function' || typeof api.restarts !== 'function') {
      throw new Error('window.__rafikxGameTest contract is missing');
    }
    const surface = gameSurface(api);
    expectState(api, 'ready');
    const readySurface = surfaceFingerprint(surface);
    const initialRestarts = Number(api.restarts());
    press('KeyR');
    await frame();
    expectState(api, 'ready');
    if (Number(api.restarts()) !== initialRestarts) throw new Error('KeyR restarted from ready');
    press('Space');
    await frame();
    expectState(api, 'playing');
    const playingSurface = surfaceFingerprint(surface);
    const playingGameplay = gameplayFingerprint(surface);
    beginGameplayObservation(surface);
    expectSurfaceChange(readySurface, playingSurface, 'ready→playing');
    press('KeyR');
    await frame();
    expectState(api, 'playing');
    if (Number(api.restarts()) !== initialRestarts) throw new Error('KeyR restarted from playing');
    press('ArrowRight');
    press('KeyD');
    await delay(160);
    await frames(4);
    release('ArrowRight');
    release('KeyD');
    expectState(api, 'playing');
    expectSurfaceChange(playingGameplay, gameplayFingerprint(surface), 'playing gameplay progress');
    const progressedSurface = surfaceFingerprint(surface);
    press('KeyP');
    await frame();
    expectState(api, 'paused');
    const pausedSurface = surfaceFingerprint(surface);
    expectSurfaceChange(progressedSurface, pausedSurface, 'playing→paused');
    press('KeyR');
    await frame();
    expectState(api, 'paused');
    if (Number(api.restarts()) !== initialRestarts) throw new Error('KeyR restarted from paused');
    press('KeyP');
    await frame();
    expectState(api, 'playing');
    const resumedSurface = surfaceFingerprint(surface);
    expectSurfaceChange(pausedSurface, resumedSurface, 'paused→playing');
    api.forceLoss();
    await frame();
    expectState(api, 'lost');
    const lostSurface = surfaceFingerprint(surface);
    expectSurfaceChange(resumedSurface, lostSurface, 'playing→lost');
    const restarts = Number(api.restarts());
    press('KeyR');
    await frame();
    expectState(api, 'ready');
    if (Number(api.restarts()) !== restarts + 1) throw new Error('restart counter did not advance');
    expectSurfaceChange(lostSurface, surfaceFingerprint(surface), 'lost→ready');
    await report('game');
  };
  window.addEventListener('load', () => setTimeout(async () => {
    try {
      await runGameContract();
    } catch (error) {
      emit('game', error?.message || error);
    } finally {
      try {
        await nativeAllSettled(pendingErrors);
        await report('ready');
      } catch (error) {
        emit('probe', error?.message || error);
      }
    }
  }, 0), { once: true });
})();"#;

pub(crate) fn entry_requires_game_contract(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains(&format!("name=\"{GAME_CONTRACT_META}\""))
        || lower.contains(&format!("name='{GAME_CONTRACT_META}'"))
}

pub(crate) fn entry_has_canvas(html: &str) -> bool {
    html.to_ascii_lowercase().contains("<canvas")
}

pub(crate) fn entry_looks_like_browser_game(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let explicit_surface = entry_has_canvas(&lower)
        || lower.contains("<svg")
        || [
            "id=\"game\"",
            "id='game'",
            "id=\"arena\"",
            "id='arena'",
            "id=\"board\"",
            "id='board'",
            "id=\"screen\"",
            "id='screen'",
            "role=\"application\"",
            "role='application'",
        ]
        .iter()
        .any(|signal| lower.contains(signal));
    let strong_game_signal = [
        "id=\"game\"",
        "id='game'",
        "id=\"board\"",
        "id='board'",
        "game.js",
        "mario",
        "platformer",
        "snake",
        "tetris",
        "pong",
        "breakout",
        "pacman",
        "flappy",
        "bird.js",
        "playable",
        "start game",
        "press space",
        "game over",
        "게임",
        "마리오",
    ]
    .iter()
    .any(|signal| lower.contains(signal));
    let lifecycle_pair = (lower.contains("lives") || lower.contains("목숨"))
        && (lower.contains("restart") || lower.contains("재시작"));
    explicit_surface && (strong_game_signal || lifecycle_pair)
}

/// 콘솔 로그(stderr)에서 런타임 오류를 추출한다 — 순수 함수(테스트 가능).
pub fn parse_console_errors(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if out.len() >= MAX_BROWSER_ERRORS {
            break;
        }
        if let Some(marker) = line.find(ERROR_MARKER) {
            let reason: String = line[marker + ERROR_MARKER.len()..]
                .trim_matches([' ', '"', ','])
                .chars()
                .take(300)
                .collect();
            if !reason.is_empty() && !out.contains(&reason) {
                out.push(reason);
            }
            continue;
        }

        let lower = line.to_ascii_lowercase();
        let web_load_error = lower.contains("blocked by cors policy")
            || lower.contains("failed to load resource")
            || lower.contains("failed to load module script")
            || lower.contains("not allowed to load local resource")
            || lower.contains("refused to execute script")
            || lower.contains("net::err_");
        let console_error = line.contains("CONSOLE")
            && (lower.contains("uncaught")
                || lower.contains("syntaxerror")
                || lower.contains("referenceerror")
                || lower.contains("typeerror")
                || lower.contains("is not defined")
                || lower.contains("is not a function"));
        if !web_load_error && !console_error {
            continue;
        }
        let detail = if console_error {
            line.find(']')
                .map(|index| line[index + 1..].trim())
                .unwrap_or(line)
        } else {
            line
        };
        let reason: String = detail
            .chars()
            .take(MAX_BROWSER_ERROR_DETAIL_CHARS)
            .collect();
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    out
}

/// 설치된 브라우저 바이너리를 찾는다 — 없으면 None.
pub fn detect_browser() -> Option<&'static str> {
    static DETECTED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DETECTED.get_or_init(find_browser).as_deref()
}

fn find_browser() -> Option<String> {
    #[cfg(windows)]
    let environment_roots = [
        "LOCALAPPDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "ProgramW6432",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(PathBuf::from)
    .collect::<Vec<_>>();
    #[cfg(not(windows))]
    let environment_roots = Vec::new();
    let candidates = browser_candidates(environment_roots);
    if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
        return Some(path.to_string_lossy().into_owned());
    }
    const PATH_NAMES: &[&str] = &[
        "google-chrome",
        "chromium-browser",
        "chromium",
        "chrome.exe",
        "msedge.exe",
        "chromium.exe",
    ];
    let paths = std::env::var_os("PATH")?;
    PATH_NAMES.iter().find_map(|name| {
        std::env::split_paths(&paths)
            .any(|directory| directory.join(name).is_file())
            .then(|| (*name).to_string())
    })
}

fn browser_candidates(environment_roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut candidates = vec![
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();
    for root in environment_roots {
        candidates.push(root.join("Google/Chrome/Application/chrome.exe"));
        candidates.push(root.join("Microsoft/Edge/Application/msedge.exe"));
    }
    candidates
}

fn network_isolation_flags(address: std::net::SocketAddr) -> [String; 4] {
    [
        format!(
            "--proxy-bypass-list=<-loopback>;http://127.0.0.1:{}",
            address.port()
        ),
        "--proxy-server=http://127.0.0.1:9".into(),
        "--host-resolver-rules=MAP * 0.0.0.0, EXCLUDE 127.0.0.1".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
    ]
}

#[derive(Clone, Copy)]
struct ProbeCompletion {
    ready: bool,
    game_sequence: bool,
}

fn evaluate_browser_output(
    success: bool,
    code: Option<i32>,
    stderr: &str,
    stderr_overflow: bool,
    server_errors: &[String],
    probe: ProbeCompletion,
    game_contract_required: bool,
) -> anyhow::Result<Vec<String>> {
    let mut errors = parse_console_errors(stderr);
    for error in server_errors {
        if !errors.contains(error) {
            errors.push(error.clone());
        }
    }
    if stderr_overflow {
        anyhow::bail!("브라우저 stderr가 수집 상한을 초과했습니다");
    }
    if !success {
        let detail = errors
            .first()
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        anyhow::bail!(
            "브라우저가 종료 코드 {}로 실패했습니다{detail}",
            code.map_or_else(|| "signal".into(), |code| code.to_string())
        );
    }
    if !probe.ready {
        anyhow::bail!("브라우저 준비 프로브가 실행되지 않았습니다");
    }
    if game_contract_required && !probe.game_sequence {
        let detail = errors
            .first()
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        anyhow::bail!("브라우저 게임 상태 전이 프로브가 완료되지 않았습니다{detail}");
    }
    Ok(errors)
}

fn inject_probe(html: &str) -> String {
    let tag = format!(r#"<script src="{PROBE_PATH}"></script>"#);
    let leading = html.len().saturating_sub(html.trim_start().len());
    let lower = html[leading..].to_ascii_lowercase();
    if lower.starts_with("<!doctype")
        && let Some(close) = html[leading..].find('>')
    {
        let at = leading + close + 1;
        let mut injected = String::with_capacity(html.len() + tag.len());
        injected.push_str(&html[..at]);
        injected.push_str(&tag);
        injected.push_str(&html[at..]);
        return injected;
    }
    format!("{tag}{html}")
}

fn percent_encode_path(path: &Path) -> String {
    let mut encoded = String::new();
    for (index, component) in path.components().enumerate() {
        let Component::Normal(part) = component else {
            continue;
        };
        if index > 0 {
            encoded.push('/');
        }
        for byte in part.to_string_lossy().as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(*byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "htm" | "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "cjs" | "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn is_web_asset(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "avif"
            | "cjs"
            | "css"
            | "gif"
            | "htm"
            | "html"
            | "ico"
            | "jpeg"
            | "jpg"
            | "js"
            | "json"
            | "m4a"
            | "mjs"
            | "mp3"
            | "mp4"
            | "ogg"
            | "otf"
            | "png"
            | "svg"
            | "ttf"
            | "wasm"
            | "wav"
            | "webm"
            | "webp"
            | "woff"
            | "woff2"
    )
}

fn has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::Normal(part) if part.to_string_lossy().starts_with('.')
        )
    })
}

fn has_sensitive_component(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(part) = component else {
            return false;
        };
        let name = part.to_string_lossy().to_ascii_lowercase();
        let stem = Path::new(&*name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&name);
        let normalized_stem = stem.replace('-', "_");
        let extension = Path::new(&*name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let sensitive_web_code = matches!(
            extension,
            "cjs" | "js" | "json" | "mjs" | "toml" | "txt" | "yaml" | "yml"
        ) && (normalized_stem == "auth"
            || normalized_stem.starts_with("auth_")
            || normalized_stem.contains("api_key")
            || normalized_stem.contains("password")
            || normalized_stem.contains("passwd")
            || normalized_stem.contains("private_key")
            || normalized_stem.contains("token"));
        normalized_stem.contains("credential")
            || normalized_stem.contains("secret")
            || sensitive_web_code
            || matches!(
                normalized_stem.as_str(),
                "id_ed25519" | "id_rsa" | "private_key"
            )
            || matches!(
                Path::new(&*name)
                    .extension()
                    .and_then(|value| value.to_str()),
                Some("key" | "pem")
            )
    })
}

fn is_discovery_excluded_name(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        "Pods" | "__pycache__" | "node_modules" | "target" | "vendor"
    )
}

fn is_changed_web_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "cjs" | "css" | "htm" | "html" | "js" | "jsx" | "mjs" | "ts" | "tsx"
    )
}

fn insert_browser_entry(
    entries: &mut BTreeSet<PathBuf>,
    root: &Path,
    candidate: &Path,
) -> anyhow::Result<()> {
    if !candidate.exists() {
        return Ok(());
    }
    let candidate = candidate.canonicalize()?;
    if !candidate.starts_with(root) || !candidate.is_file() {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    entries.insert(candidate);
    if entries.len() > MAX_BROWSER_ENTRIES {
        anyhow::bail!("브라우저 엔트리 수가 검증 상한을 넘었습니다");
    }
    Ok(())
}

fn project_root_for(root: &Path, path: &Path) -> Option<PathBuf> {
    let mut directory = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    while directory.starts_with(root) {
        if directory.join("package.json").is_file() {
            return Some(directory);
        }
        if directory == root || !directory.pop() {
            break;
        }
    }
    None
}

fn resolve_html_reference(root: &Path, entry: &Path, raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.starts_with('#')
        || raw.starts_with("//")
        || raw.contains("://")
        || raw.to_ascii_lowercase().starts_with("data:")
        || raw.to_ascii_lowercase().starts_with("javascript:")
    {
        return None;
    }
    let raw = raw.split(['?', '#']).next().unwrap_or_default();
    let mut relative = if raw.starts_with('/') {
        PathBuf::new()
    } else {
        entry.parent()?.strip_prefix(root).ok()?.to_path_buf()
    };
    for component in Path::new(raw.trim_start_matches('/')).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !relative.pop() {
                    return None;
                }
            }
            Component::Normal(part) => relative.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(root.join(relative))
}

#[derive(Clone)]
enum ReferenceToken {
    Identifier(String),
    Punctuation(u8),
    ControlClose,
    Value,
}

fn push_reference_token(tokens: &mut Vec<ReferenceToken>, token: ReferenceToken) {
    if tokens.len() == 3 {
        tokens.remove(0);
    }
    tokens.push(token);
}

fn quoted_value(text: &str, start: usize, quote: u8) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor = (cursor + 2).min(bytes.len());
        } else if bytes[cursor] == quote {
            return Some((text[start..cursor].to_string(), cursor + 1));
        } else {
            cursor += 1;
        }
    }
    None
}

fn reference_literal_context(tokens: &[ReferenceToken], css: bool) -> bool {
    let last_identifier = |offset: usize| {
        tokens
            .len()
            .checked_sub(offset + 1)
            .and_then(|index| match &tokens[index] {
                ReferenceToken::Identifier(value) => Some(value.as_str()),
                _ => None,
            })
    };
    if matches!(last_identifier(0), Some("import" | "from")) {
        return true;
    }
    let called = matches!(tokens.last(), Some(ReferenceToken::Punctuation(b'(')))
        .then(|| last_identifier(1))
        .flatten();
    let assigned = matches!(
        tokens.last(),
        Some(ReferenceToken::Punctuation(b'=' | b':'))
    )
    .then(|| last_identifier(1))
    .flatten();
    if css {
        called == Some("url")
    } else {
        matches!(
            called,
            Some("audio" | "fetch" | "import" | "require" | "url" | "worker" | "sharedworker")
        ) || matches!(assigned, Some("href" | "src"))
    }
}

fn regex_literal_can_start(tokens: &[ReferenceToken]) -> bool {
    match tokens.last() {
        None => true,
        Some(ReferenceToken::Punctuation(value)) => {
            matches!(
                *value,
                b'(' | b'['
                    | b'{'
                    | b'='
                    | b':'
                    | b','
                    | b';'
                    | b'!'
                    | b'?'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'%'
                    | b'&'
                    | b'|'
                    | b'^'
                    | b'~'
                    | b'<'
                    | b'>'
            )
        }
        Some(ReferenceToken::Identifier(value)) => {
            matches!(
                value.as_str(),
                "await"
                    | "case"
                    | "delete"
                    | "do"
                    | "else"
                    | "in"
                    | "instanceof"
                    | "new"
                    | "of"
                    | "return"
                    | "throw"
                    | "typeof"
                    | "void"
                    | "yield"
            )
        }
        Some(ReferenceToken::ControlClose) => true,
        Some(ReferenceToken::Value) => false,
    }
}

fn skip_regex_literal(bytes: &[u8], start: usize) -> usize {
    let mut cursor = start + 1;
    let mut character_class = false;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'[' => {
                character_class = true;
                cursor += 1;
            }
            b']' => {
                character_class = false;
                cursor += 1;
            }
            b'/' if !character_class => {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                    cursor += 1;
                }
                return cursor;
            }
            b'\n' | b'\r' => return start + 1,
            _ => cursor += 1,
        }
    }
    start + 1
}

fn code_reference_values(text: &str, css: bool) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut values = Vec::new();
    let mut tokens = Vec::new();
    let mut control_parentheses = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor += 2;
            while cursor + 1 < bytes.len() && !(bytes[cursor] == b'*' && bytes[cursor + 1] == b'/')
            {
                cursor += 1;
            }
            cursor = (cursor + 2).min(bytes.len());
            continue;
        }
        if !css && bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor += 2;
            while cursor < bytes.len() && !matches!(bytes[cursor], b'\n' | b'\r') {
                cursor += 1;
            }
            continue;
        }
        if !css && bytes[cursor] == b'/' && regex_literal_can_start(&tokens) {
            cursor = skip_regex_literal(bytes, cursor);
            push_reference_token(&mut tokens, ReferenceToken::Value);
            continue;
        }
        if matches!(bytes[cursor], b'\'' | b'"' | b'`') {
            let quote = bytes[cursor];
            if let Some((value, next)) = quoted_value(text, cursor + 1, quote) {
                if reference_literal_context(&tokens, css) {
                    values.push(value);
                }
                cursor = next;
                push_reference_token(&mut tokens, ReferenceToken::Value);
                continue;
            }
        }
        if bytes[cursor].is_ascii_alphabetic() || matches!(bytes[cursor], b'_' | b'$') {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric()
                    || matches!(bytes[cursor], b'_' | b'$' | b'-'))
            {
                cursor += 1;
            }
            push_reference_token(
                &mut tokens,
                ReferenceToken::Identifier(text[start..cursor].to_ascii_lowercase()),
            );
            continue;
        }
        let token = match bytes[cursor] {
            b'(' => {
                let control = matches!(
                    tokens.last(),
                    Some(ReferenceToken::Identifier(value))
                        if matches!(value.as_str(), "catch" | "for" | "if" | "switch" | "while" | "with")
                );
                control_parentheses.push(control);
                ReferenceToken::Punctuation(b'(')
            }
            b')' if control_parentheses.pop().unwrap_or(false) => ReferenceToken::ControlClose,
            punctuation => ReferenceToken::Punctuation(punctuation),
        };
        push_reference_token(&mut tokens, token);
        cursor += 1;
    }
    values
}

fn html_tag_reference_values(tag: &str) -> Vec<String> {
    let bytes = tag.as_bytes();
    let mut values = Vec::new();
    let mut cursor = 1usize;
    while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'>' {
        cursor += 1;
    }
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'-' | b'_'))
        {
            cursor += 1;
        }
        if name_start == cursor {
            cursor += 1;
            continue;
        }
        let name = &tag[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value = if let Some(quote @ (b'\'' | b'"')) = bytes.get(cursor).copied() {
            quoted_value(tag, cursor + 1, quote).map(|(value, next)| {
                cursor = next;
                value
            })
        } else {
            let start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            (start < cursor).then(|| tag[start..cursor].to_string())
        };
        if matches!(name.to_ascii_lowercase().as_str(), "src" | "href")
            && let Some(value) = value
        {
            values.push(value);
        }
    }
    values
}

fn html_reference_values(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut values = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"<!--") {
            cursor = lower[cursor + 4..]
                .find("-->")
                .map(|offset| cursor + 4 + offset + 3)
                .unwrap_or(bytes.len());
            continue;
        }
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        let mut quote = None;
        while end < bytes.len() {
            if let Some(active) = quote {
                if bytes[end] == b'\\' {
                    end = (end + 2).min(bytes.len());
                    continue;
                }
                if bytes[end] == active {
                    quote = None;
                }
            } else if matches!(bytes[end], b'\'' | b'"') {
                quote = Some(bytes[end]);
            } else if bytes[end] == b'>' {
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        let tag = &text[cursor..=end];
        values.extend(html_tag_reference_values(tag));
        let tag_name = tag[1..]
            .trim_start_matches('/')
            .split(|character: char| character.is_ascii_whitespace() || character == '>')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(tag_name.as_str(), "script" | "style") && !tag.starts_with("</") {
            let closing = format!("</{tag_name}");
            if let Some(offset) = lower[end + 1..].find(&closing) {
                let content_end = end + 1 + offset;
                values.extend(code_reference_values(
                    &text[end + 1..content_end],
                    tag_name == "style",
                ));
                cursor = content_end;
                continue;
            }
        }
        cursor = end + 1;
    }
    values
}

fn local_references(root: &Path, source: &Path, text: &str) -> BTreeSet<PathBuf> {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let values = match extension.as_str() {
        "htm" | "html" => html_reference_values(text),
        "css" => code_reference_values(text, true),
        _ => code_reference_values(text, false),
    };
    values
        .into_iter()
        .filter_map(|value| resolve_html_reference(root, source, &value))
        .collect()
}

fn is_reference_graph_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "cjs" | "css" | "htm" | "html" | "js" | "jsx" | "mjs" | "ts" | "tsx"
    )
}

fn entry_reaches_changed_sources(
    root: &Path,
    entry: &Path,
    changed_sources: &[PathBuf],
    started: Instant,
) -> anyhow::Result<Vec<PathBuf>> {
    let targets = changed_sources
        .iter()
        .map(|source| (root.join(source), source))
        .collect::<Vec<_>>();
    let mut matched = BTreeSet::new();
    let mut pending = vec![entry.to_path_buf()];
    let mut visited = BTreeSet::new();
    let mut inspected_bytes = 0u64;

    while let Some(source) = pending.pop() {
        if started.elapsed() > MAX_DISCOVERY_DURATION {
            anyhow::bail!("브라우저 참조 그래프 탐색 시간 상한을 넘었습니다");
        }
        if !visited.insert(source.clone()) {
            continue;
        }
        if visited.len() > MAX_REFERENCE_GRAPH_ENTRIES {
            anyhow::bail!("브라우저 참조 그래프 항목 수가 상한을 넘었습니다");
        }
        if !source.starts_with(root) {
            anyhow::bail!("브라우저 참조 그래프가 워크스페이스 밖을 가리킵니다");
        }
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) | Err(_) => continue,
        };
        if metadata.len() > MAX_DISCOVERY_HTML_BYTES {
            anyhow::bail!("브라우저 참조 그래프 파일이 상한을 넘었습니다");
        }
        inspected_bytes = inspected_bytes.saturating_add(metadata.len());
        if inspected_bytes > MAX_REFERENCE_GRAPH_BYTES {
            anyhow::bail!("브라우저 참조 그래프 합계가 상한을 넘었습니다");
        }
        let text = std::fs::read_to_string(&source).map_err(|error| {
            anyhow::anyhow!("브라우저 참조 그래프 파일을 읽을 수 없습니다: {error}")
        })?;
        for reference in local_references(root, &source, &text) {
            let reference = reference.canonicalize().unwrap_or(reference);
            if !reference.starts_with(root) {
                anyhow::bail!("브라우저 참조 그래프가 워크스페이스 밖을 가리킵니다");
            }
            for (absolute, changed) in &targets {
                if reference == *absolute {
                    matched.insert((*changed).clone());
                }
            }
            if reference.starts_with(root) && is_reference_graph_source(&reference) {
                pending.push(reference);
            }
        }
    }
    Ok(matched.into_iter().collect())
}

#[cfg(test)]
pub(crate) fn discover_entries(
    workspace: &Path,
    changed: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    discover_entries_with_mode(workspace, changed, false)
}

pub(crate) fn discover_entries_for_task(
    workspace: &Path,
    changed: &[String],
    exhaustive: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    discover_entries_with_mode(workspace, changed, exhaustive)
}

fn discover_entries_with_mode(
    workspace: &Path,
    changed: &[String],
    exhaustive: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let root = workspace.canonicalize()?;
    let started = Instant::now();
    let mut entries = BTreeSet::new();
    let mut changed_sources = Vec::new();
    let mut covered_sources = BTreeSet::new();
    for file in changed {
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            anyhow::bail!("워크스페이스 밖 변경 경로: {file}");
        }
        if !is_changed_web_source(relative) {
            continue;
        }
        let normalized_relative = root
            .join(relative)
            .canonicalize()
            .ok()
            .and_then(|path| path.strip_prefix(&root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| relative.to_path_buf());
        changed_sources.push(normalized_relative.clone());
        if matches!(
            relative
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "htm" | "html"
        ) {
            insert_browser_entry(&mut entries, &root, &root.join(&normalized_relative))?;
            covered_sources.insert(normalized_relative);
        }
    }
    if changed_sources.is_empty() {
        return Ok(entries.into_iter().collect());
    }

    for source in &changed_sources {
        if covered_sources.contains(source) {
            continue;
        }
        let joined = root.join(source);
        let Some(parent) = joined.parent() else {
            continue;
        };
        let Ok(mut directory) = parent.canonicalize() else {
            continue;
        };
        'ancestor: while directory.starts_with(&root) {
            for candidate in [
                directory.join("index.html"),
                directory.join("public/index.html"),
                directory.join("www/index.html"),
            ] {
                if candidate.is_file()
                    && !entry_reaches_changed_sources(
                        &root,
                        &candidate,
                        std::slice::from_ref(source),
                        started,
                    )?
                    .is_empty()
                {
                    insert_browser_entry(&mut entries, &root, &candidate)?;
                    covered_sources.insert(source.clone());
                    break 'ancestor;
                }
            }
            if directory == root || !directory.pop() {
                break;
            }
        }
    }

    for source in &changed_sources {
        if covered_sources.contains(source) {
            continue;
        }
        let Some(project_root) = project_root_for(&root, &root.join(source)) else {
            continue;
        };
        for candidate in [
            project_root.join("index.html"),
            project_root.join("public/index.html"),
            project_root.join("src/index.html"),
            project_root.join("www/index.html"),
            project_root.join("dist/index.html"),
            project_root.join("build/index.html"),
        ] {
            if candidate.is_file()
                && !entry_reaches_changed_sources(
                    &root,
                    &candidate,
                    std::slice::from_ref(source),
                    started,
                )?
                .is_empty()
            {
                insert_browser_entry(&mut entries, &root, &candidate)?;
                covered_sources.insert(source.clone());
                break;
            }
        }
    }
    if !exhaustive
        && changed_sources
            .iter()
            .all(|source| covered_sources.contains(source))
    {
        return Ok(entries.into_iter().collect());
    }

    let filter_root = root.clone();
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .filter_entry(move |item| {
            if item.depth() == 0 {
                return true;
            }
            let relative = item
                .path()
                .strip_prefix(&filter_root)
                .unwrap_or(item.path());
            !has_hidden_component(relative)
                && !has_sensitive_component(relative)
                && !is_discovery_excluded_name(item.file_name())
        })
        .build();
    let mut inspected_html_bytes = 0u64;
    for (index, item) in walker.enumerate() {
        if index >= MAX_DISCOVERY_ENTRIES {
            anyhow::bail!("브라우저 엔트리 탐색 항목 수가 상한을 넘었습니다");
        }
        if started.elapsed() > MAX_DISCOVERY_DURATION {
            anyhow::bail!("브라우저 엔트리 탐색 시간 상한을 넘었습니다");
        }
        let item = item.map_err(|error| anyhow::anyhow!("브라우저 엔트리 탐색 실패: {error}"))?;
        if item.file_type().is_some_and(|kind| kind.is_file())
            && matches!(
                item.path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "htm" | "html"
            )
        {
            let metadata = item
                .metadata()
                .map_err(|error| anyhow::anyhow!("브라우저 엔트리 상태 확인 실패: {error}"))?;
            if metadata.len() <= MAX_DISCOVERY_HTML_BYTES
                && inspected_html_bytes.saturating_add(metadata.len())
                    <= MAX_DISCOVERY_TOTAL_HTML_BYTES
            {
                inspected_html_bytes = inspected_html_bytes.saturating_add(metadata.len());
                let referenced =
                    entry_reaches_changed_sources(&root, item.path(), &changed_sources, started)?;
                if !referenced.is_empty() {
                    insert_browser_entry(&mut entries, &root, item.path())?;
                    covered_sources.extend(referenced);
                }
            }
        }
    }
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let uncovered = changed_sources
        .iter()
        .filter(|source| !covered_sources.contains(*source))
        .take(8)
        .map(|source| source.display().to_string())
        .collect::<Vec<_>>();
    if !uncovered.is_empty() {
        anyhow::bail!(
            "변경된 웹 소스를 실행할 HTML 엔트리를 찾지 못했습니다: {}",
            uncovered.join(", ")
        );
    }
    Ok(entries.into_iter().collect())
}

fn safe_extra_browser_flag(flag: &str) -> bool {
    let flag = flag.to_ascii_lowercase();
    flag.starts_with('-')
        && ![
            "allow-file-access-from-files",
            "allow-running-insecure-content",
            "disable-web-security",
            "host-resolver-rules",
            "host-rules",
            "proxy",
            "remote-debugging",
            "user-data-dir",
        ]
        .iter()
        .any(|blocked| flag.contains(blocked))
}

fn stage_web_root(workspace: &Path, entry_html: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    stage_web_root_with_limits(
        workspace,
        entry_html,
        stage,
        MAX_STAGED_ENTRIES,
        MAX_STAGING_DURATION,
    )
}

fn stage_web_root_with_limits(
    workspace: &Path,
    entry_html: &Path,
    stage: &Path,
    max_entries: usize,
    max_duration: Duration,
) -> anyhow::Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    let entry = entry_html.canonicalize()?;
    if !entry.starts_with(&workspace) || !entry.is_file() {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    let source_root = project_root_for(&workspace, &entry).unwrap_or_else(|| workspace.clone());
    std::fs::create_dir_all(stage)?;
    let mut staged_bytes = 0u64;
    let mut entries = 0usize;
    let started = Instant::now();
    let mut pending = vec![entry.clone()];
    let mut visited = BTreeSet::new();
    while let Some(source) = pending.pop() {
        if started.elapsed() > max_duration {
            anyhow::bail!("브라우저 자산 스테이징 시간 상한을 넘었습니다");
        }
        if !visited.insert(source.clone()) {
            continue;
        }
        let relative = source
            .strip_prefix(&source_root)
            .map_err(|_| anyhow::anyhow!("브라우저 자산이 프로젝트 밖을 가리킵니다"))?;
        if has_hidden_component(relative) || has_sensitive_component(relative) {
            anyhow::bail!(
                "민감한 브라우저 자산 참조를 차단했습니다: {}",
                relative.display()
            );
        }
        if relative
            .components()
            .any(|component| matches!(component, Component::Normal(name) if is_discovery_excluded_name(name)))
        {
            anyhow::bail!("제외된 브라우저 자산 참조를 차단했습니다: {}", relative.display());
        }
        let resolved = source.canonicalize().map_err(|error| {
            anyhow::anyhow!(
                "브라우저 자산 참조를 확인할 수 없습니다({}): {error}",
                relative.display()
            )
        })?;
        if !resolved.starts_with(&source_root) || !resolved.is_file() {
            anyhow::bail!(
                "브라우저 자산이 프로젝트 밖을 가리킵니다: {}",
                relative.display()
            );
        }
        let resolved_relative = resolved
            .strip_prefix(&source_root)
            .map_err(|_| anyhow::anyhow!("브라우저 자산이 프로젝트 밖을 가리킵니다"))?;
        if has_hidden_component(resolved_relative) || has_sensitive_component(resolved_relative) {
            anyhow::bail!(
                "민감한 브라우저 자산 대상을 차단했습니다: {}",
                relative.display()
            );
        }
        if resolved_relative.components().any(
            |component| matches!(component, Component::Normal(name) if is_discovery_excluded_name(name)),
        ) {
            anyhow::bail!(
                "제외된 브라우저 자산 대상을 차단했습니다: {}",
                relative.display()
            );
        }
        if !is_web_asset(&source) || !is_web_asset(&resolved) {
            anyhow::bail!(
                "허용되지 않은 브라우저 자산 형식입니다: {}",
                relative.display()
            );
        }
        entries = entries.saturating_add(1);
        if entries > max_entries {
            anyhow::bail!("브라우저 자산 항목 수가 상한을 넘었습니다");
        }
        let metadata = resolved.metadata()?;
        if metadata.len() > MAX_STAGED_FILE_BYTES {
            anyhow::bail!(
                "브라우저 자산이 파일 상한을 넘었습니다: {}",
                relative.display()
            );
        }
        staged_bytes = staged_bytes.saturating_add(metadata.len());
        if staged_bytes > MAX_STAGED_TOTAL_BYTES {
            anyhow::bail!("브라우저 자산 합계가 상한을 넘었습니다");
        }
        let destination = stage.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&resolved, destination)?;
        if is_reference_graph_source(&source) {
            let text = std::fs::read_to_string(&resolved).map_err(|error| {
                anyhow::anyhow!(
                    "브라우저 자산 참조 그래프를 읽을 수 없습니다({}): {error}",
                    relative.display()
                )
            })?;
            for reference in local_references(&source_root, &source, &text) {
                let reference_relative = reference.strip_prefix(&source_root).map_err(|_| {
                    anyhow::anyhow!("브라우저 자산 참조가 프로젝트 밖을 가리킵니다")
                })?;
                if has_hidden_component(reference_relative)
                    || has_sensitive_component(reference_relative)
                {
                    anyhow::bail!(
                        "민감한 브라우저 자산 참조를 차단했습니다: {}",
                        reference_relative.display()
                    );
                }
                if std::fs::symlink_metadata(&reference).is_ok_and(|metadata| {
                    metadata.file_type().is_file() || metadata.file_type().is_symlink()
                }) {
                    pending.push(reference);
                }
            }
        }
        if started.elapsed() > max_duration {
            anyhow::bail!("브라우저 자산 스테이징 시간 상한을 넘었습니다");
        }
    }
    let relative_entry = entry.strip_prefix(&source_root)?;
    let staged_entry = stage.join(relative_entry);
    if !staged_entry.is_file() {
        anyhow::bail!("브라우저 엔트리를 격리 스테이징하지 못했습니다");
    }
    Ok(staged_entry)
}

fn append_bounded_stderr(log: &mut String, chunk: &str) -> bool {
    let remaining = MAX_BROWSER_STDERR_BYTES.saturating_sub(log.len());
    if remaining == 0 {
        return !chunk.is_empty();
    }
    let overflow = chunk.len() > remaining;
    let mut end = remaining.min(chunk.len());
    while end > 0 && !chunk.is_char_boundary(end) {
        end -= 1;
    }
    log.push_str(&chunk[..end]);
    overflow
}

struct SmokeResources {
    server: Option<tokio::task::JoinHandle<()>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    run_dir: PathBuf,
}

#[derive(Clone)]
struct ProbeServerState {
    token: Arc<str>,
    script_served: Arc<AtomicBool>,
    ready: tokio::sync::watch::Sender<bool>,
    sequence: Arc<Mutex<ProbeSequence>>,
}

#[derive(Default)]
struct ProbeSequence {
    game: bool,
    completed: bool,
    errors: std::collections::BTreeSet<String>,
}

struct ProbeReceipt {
    ready: tokio::sync::watch::Receiver<bool>,
    sequence: Arc<Mutex<ProbeSequence>>,
}

enum ProbeReceiptDecision {
    Accepted(Option<String>),
    Conflict,
    Unavailable,
}

fn accept_probe_receipt(
    probe: &ProbeServerState,
    kind: &str,
    decoded_error: Option<String>,
) -> ProbeReceiptDecision {
    let Ok(mut sequence) = probe.sequence.lock() else {
        return ProbeReceiptDecision::Unavailable;
    };
    match kind {
        "game" if !sequence.completed && !sequence.game => {
            sequence.game = true;
            ProbeReceiptDecision::Accepted(None)
        }
        "ready" if !sequence.completed => {
            sequence.completed = true;
            let _ = probe.ready.send(true);
            ProbeReceiptDecision::Accepted(None)
        }
        "error" if !sequence.completed => {
            let Some(detail) = decoded_error else {
                return ProbeReceiptDecision::Conflict;
            };
            let detail: String = detail
                .chars()
                .take(MAX_BROWSER_ERROR_DETAIL_CHARS)
                .collect();
            if sequence.errors.len() >= MAX_BROWSER_ERRORS
                || !sequence.errors.insert(detail.clone())
            {
                return ProbeReceiptDecision::Conflict;
            }
            ProbeReceiptDecision::Accepted(Some(detail))
        }
        _ => ProbeReceiptDecision::Conflict,
    }
}

impl SmokeResources {
    fn new(run_dir: PathBuf) -> Self {
        Self {
            server: None,
            reader: None,
            run_dir,
        }
    }
}

impl Drop for SmokeResources {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        let _ = std::fs::remove_dir_all(&self.run_dir);
    }
}

async fn await_stderr_reader(resources: &mut SmokeResources) -> anyhow::Result<()> {
    let Some(mut reader) = resources.reader.take() else {
        return Ok(());
    };
    match tokio::time::timeout(Duration::from_secs(2), &mut reader).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => anyhow::bail!("브라우저 stderr 수집 작업 실패: {error}"),
        Err(_) => {
            reader.abort();
            let _ = reader.await;
            anyhow::bail!("브라우저 stderr 수집 종료 시간 초과")
        }
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{SECURITY_HEADERS}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await
}

async fn serve_request(
    mut stream: TcpStream,
    root: PathBuf,
    entry: PathBuf,
    errors: Arc<Mutex<Vec<String>>>,
    probe: ProbeServerState,
) -> std::io::Result<()> {
    let mut request = vec![0u8; 16 * 1024];
    let read = stream.read(&mut request).await?;
    let request = String::from_utf8_lossy(&request[..read]);
    let Some((method, raw_path)) = request.lines().next().and_then(|line| {
        let mut fields = line.split_whitespace();
        Some((
            fields.next()?,
            fields.next()?.split('?').next().unwrap_or(""),
        ))
    }) else {
        return write_response(&mut stream, "400 Bad Request", "text/plain", b"bad request").await;
    };
    if raw_path == PROBE_PATH {
        if probe.script_served.swap(true, Ordering::AcqRel) {
            return write_response(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"probe already served",
            )
            .await;
        }
        let script = PROBE_SCRIPT.replace(PROBE_TOKEN_PLACEHOLDER, &probe.token);
        return write_response(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            script.as_bytes(),
        )
        .await;
    }
    if let Some(receipt) = raw_path.strip_prefix(PROBE_RESULT_PATH) {
        let mut parts = receipt.split('/');
        let token = parts.next().unwrap_or_default();
        let kind = parts.next().unwrap_or_default();
        let detail = parts.next();
        let valid = method == "POST"
            && token == probe.token.as_ref()
            && parts.next().is_none()
            && matches!(kind, "ready" | "game" | "error")
            && (kind == "error") == detail.is_some();
        if !valid {
            return write_response(&mut stream, "403 Forbidden", "text/plain", b"forbidden").await;
        }
        let decoded_error = detail.and_then(percent_decode_path);
        if kind == "error" && decoded_error.is_none() {
            return write_response(&mut stream, "403 Forbidden", "text/plain", b"forbidden").await;
        }
        let accepted_error = match accept_probe_receipt(&probe, kind, decoded_error) {
            ProbeReceiptDecision::Accepted(detail) => detail,
            ProbeReceiptDecision::Conflict => {
                return write_response(
                    &mut stream,
                    "409 Conflict",
                    "text/plain",
                    b"duplicate or out-of-order receipt",
                )
                .await;
            }
            ProbeReceiptDecision::Unavailable => {
                return write_response(
                    &mut stream,
                    "500 Internal Server Error",
                    "text/plain",
                    b"probe state unavailable",
                )
                .await;
            }
        };
        if let Some(detail) = accepted_error
            && let Ok(mut errors) = errors.lock()
            && errors.len() < MAX_BROWSER_ERRORS
        {
            errors.push(detail);
        }
        return write_response(&mut stream, "204 No Content", "text/plain", b"").await;
    }
    if raw_path == "/favicon.ico" {
        return write_response(&mut stream, "204 No Content", "image/x-icon", b"").await;
    }

    let decoded = percent_decode_path(raw_path.trim_start_matches('/'));
    let Some(decoded) = decoded else {
        return write_response(&mut stream, "400 Bad Request", "text/plain", b"bad path").await;
    };
    let relative = Path::new(&decoded);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return write_response(&mut stream, "403 Forbidden", "text/plain", b"forbidden").await;
    }
    let requested = root.join(relative);
    let resolved = match requested.canonicalize() {
        Ok(path) if path.starts_with(&root) && path.is_file() => path,
        _ => {
            if let Ok(mut errors) = errors.lock()
                && errors.len() < MAX_BROWSER_ERRORS
            {
                errors.push(format!("HTTP 404: /{decoded}"));
            }
            return write_response(&mut stream, "404 Not Found", "text/plain", b"not found").await;
        }
    };
    let mut body = tokio::fs::read(&resolved).await?;
    if resolved == entry {
        let html = String::from_utf8_lossy(&body);
        body = inject_probe(&html).into_bytes();
    }
    write_response(&mut stream, "200 OK", content_type(&resolved), &body).await
}

async fn start_server(
    workspace: &Path,
    entry_html: &Path,
) -> anyhow::Result<(
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    PathBuf,
    ProbeReceipt,
)> {
    let root = workspace.canonicalize()?;
    let entry = entry_html.canonicalize()?;
    if !entry.starts_with(&root) {
        anyhow::bail!("브라우저 엔트리가 워크스페이스 밖에 있습니다");
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let (ready, ready_receipt) = tokio::sync::watch::channel(false);
    let sequence = Arc::new(Mutex::new(ProbeSequence::default()));
    let probe = ProbeServerState {
        token: Arc::from(crate::auth::random_hex(16)),
        script_served: Arc::new(AtomicBool::new(false)),
        ready,
        sequence: sequence.clone(),
    };
    let receipt = ProbeReceipt {
        ready: ready_receipt,
        sequence,
    };
    let server_errors = errors.clone();
    let server_root = root.clone();
    let server_entry = entry.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let root = server_root.clone();
            let entry = server_entry.clone();
            let errors = server_errors.clone();
            let probe = probe.clone();
            tokio::spawn(async move {
                let _ = serve_request(stream, root, entry, errors, probe).await;
            });
        }
    });
    Ok((address, task, errors, root, receipt))
}

/// 엔트리 HTML 을 실제 브라우저로 로드해 콘솔 오류를 수집한다.
/// 브라우저가 없으면 Ok(None) — 호출자가 HTML 런타임 검증 불가로 처리한다.
pub async fn smoke_test(entry_html: &std::path::Path) -> anyhow::Result<Option<Vec<String>>> {
    let entry = entry_html.canonicalize()?;
    let current = std::env::current_dir()?.canonicalize()?;
    let workspace = if entry.starts_with(&current) {
        current
    } else {
        entry
            .parent()
            .ok_or_else(|| anyhow::anyhow!("브라우저 엔트리의 상위 폴더가 없습니다"))?
            .to_path_buf()
    };
    smoke_test_in_workspace(&workspace, &entry).await
}

pub(crate) async fn smoke_test_in_workspace(
    workspace: &std::path::Path,
    entry_html: &std::path::Path,
) -> anyhow::Result<Option<Vec<String>>> {
    smoke_test_in_workspace_with_contract(workspace, entry_html, false).await
}

pub(crate) async fn smoke_test_in_workspace_with_contract(
    workspace: &std::path::Path,
    entry_html: &std::path::Path,
    task_requires_game_contract: bool,
) -> anyhow::Result<Option<Vec<String>>> {
    let html = std::fs::read_to_string(entry_html)?;
    let has_contract = entry_requires_game_contract(&html);
    let game_contract_required =
        task_requires_game_contract || has_contract || entry_looks_like_browser_game(&html);
    if game_contract_required && !has_contract {
        anyhow::bail!(
            "브라우저 게임 계약 meta가 없습니다: <meta name=\"{GAME_CONTRACT_META}\" content=\"v1\">"
        );
    }
    let Some(browser) = detect_browser() else {
        return Ok(None);
    };
    let run_dir =
        std::env::temp_dir().join(format!("rafikx-browser-smoke-{}", crate::db::Db::new_id()));
    std::fs::create_dir_all(&run_dir)?;
    let mut resources = SmokeResources::new(run_dir.clone());
    let stage_dir = run_dir.join("web");
    let profile_dir = run_dir.join("profile");
    let staged_entry = stage_web_root(workspace, entry_html, &stage_dir)?;
    let (address, server, server_errors, root, mut probe_receipt) =
        start_server(&stage_dir, &staged_entry).await?;
    resources.server = Some(server);
    let entry = staged_entry.canonicalize()?;
    let relative = entry.strip_prefix(&root)?;
    let url = format!("http://{address}/{}", percent_encode_path(relative));
    std::fs::create_dir_all(&profile_dir)?;
    let profile_flag = format!("--user-data-dir={}", profile_dir.display());
    // --no-sandbox 는 넣지 않는다 — 이 플래그가 있으면 콘솔 로그가 캡처되지
    // 않는다(실측). 컨테이너 root 등 필요한 환경은 RAFIKX_BROWSER_EXTRA_FLAGS 로
    // 추가 플래그를 넣는다 (공백 구분).
    let extra: Vec<String> = std::env::var("RAFIKX_BROWSER_EXTRA_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|flag| safe_extra_browser_flag(flag))
        .map(str::to_string)
        .collect();
    let isolation = network_isolation_flags(address);
    let mut command = tokio::process::Command::new(browser);
    command
        .args([
            "--headless",
            "--disable-gpu",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--enable-logging=stderr",
            "--metrics-recording-only",
            "--no-default-browser-check",
            "--no-first-run",
            "--v=0",
        ])
        .arg(profile_flag)
        .args(&extra)
        .args(&isolation)
        .arg(&url)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let (child, process_scope) = crate::process_tree::spawn_scoped(&mut command)
        .map_err(|error| anyhow::anyhow!("브라우저 실행 실패: {error}"))?;
    let mut process = crate::process_tree::ScopedProcess::new(child, process_scope);
    let Some(stderr) = process
        .child_mut()
        .map_err(anyhow::Error::msg)?
        .stderr
        .take()
    else {
        process.terminate().await.map_err(anyhow::Error::msg)?;
        anyhow::bail!("브라우저 stderr를 열 수 없습니다");
    };
    let stderr_log = Arc::new(Mutex::new(String::new()));
    let reader_log = stderr_log.clone();
    let stderr_overflow = Arc::new(AtomicBool::new(false));
    let reader_overflow = stderr_overflow.clone();
    let reader = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buffer = [0u8; 8 * 1024];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            if let Ok(mut log) = reader_log.lock()
                && append_bounded_stderr(&mut log, &String::from_utf8_lossy(&buffer[..read]))
            {
                reader_overflow.store(true, Ordering::Relaxed);
            }
        }
    });
    resources.reader = Some(reader);

    let deadline = tokio::time::sleep(std::time::Duration::from_secs(15));
    tokio::pin!(deadline);
    let mut early_status = None;
    let ready = loop {
        if *probe_receipt.ready.borrow() {
            break true;
        }
        if let Some(status) = process
            .child_mut()
            .map_err(anyhow::Error::msg)?
            .try_wait()?
        {
            early_status = Some(status);
            break false;
        }
        tokio::select! {
            _ = &mut deadline => break false,
            changed = probe_receipt.ready.changed() => {
                if changed.is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    };
    let (success, code) = if ready {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match process
            .child_mut()
            .map_err(anyhow::Error::msg)?
            .try_wait()?
        {
            Some(status) => (status.success(), status.code()),
            None => (true, Some(0)),
        }
    } else if let Some(status) = early_status {
        (status.success(), status.code())
    } else {
        process.terminate().await.map_err(anyhow::Error::msg)?;
        let _ = await_stderr_reader(&mut resources).await;
        anyhow::bail!("브라우저 준비 프로브 시간 초과 (15초)");
    };
    process.terminate().await.map_err(anyhow::Error::msg)?;
    await_stderr_reader(&mut resources).await?;
    let captured_server_errors = server_errors
        .lock()
        .map(|errors| errors.clone())
        .unwrap_or_default();
    let stderr = stderr_log.lock().map(|log| log.clone()).unwrap_or_default();
    let probe_ready = *probe_receipt.ready.borrow();
    let game_sequence_ready = probe_receipt
        .sequence
        .lock()
        .map(|sequence| sequence.game)
        .unwrap_or(false);
    Ok(Some(evaluate_browser_output(
        success,
        code,
        &stderr,
        stderr_overflow.load(Ordering::Relaxed),
        &captured_server_errors,
        ProbeCompletion {
            ready: probe_ready,
            game_sequence: game_sequence_ready,
        },
        game_contract_required,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn probe_request(state: ProbeServerState, request: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(serve_request(
            server,
            PathBuf::new(),
            PathBuf::new(),
            errors,
            state,
        ));
        client.write_all(request.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        task.await.unwrap().unwrap();
        String::from_utf8(response).unwrap()
    }

    async fn canvas_contract_fixture(
        label: &str,
        drawing: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-canvas-{label}-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root)?;
        let entry = root.join("index.html");
        let html = [
            r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"></head><body><canvas id="game" width="320" height="180"></canvas><script>
const canvas = document.querySelector('#game');
const context = canvas.getContext('2d');
const game = { mode: 'ready', restarts: 0, tick: 0 };
"#,
            drawing,
            r#"
const drawState = () => {
  context.setTransform(1, 0, 0, 1, 0, 0);
  context.globalAlpha = 1;
  context.clearRect(0, 0, canvas.width, canvas.height);
  if (game.mode === 'playing') drawPlaying(game.tick);
  else {
    context.fillStyle = game.mode === 'ready' ? '#14532d' : game.mode === 'paused' ? '#854d0e' : '#7f1d1d';
    context.fillRect(0, 0, canvas.width, canvas.height);
  }
};
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && game.mode === 'ready') game.mode = 'playing';
  else if (event.code === 'KeyP' && game.mode === 'playing') game.mode = 'paused';
  else if (event.code === 'KeyP' && game.mode === 'paused') game.mode = 'playing';
  else if (event.code === 'KeyR' && game.mode === 'lost') {
    game.mode = 'ready'; game.restarts += 1; game.tick = 0;
  }
  drawState();
});
const animate = () => {
  if (game.mode === 'playing') { game.tick += 1; drawPlaying(game.tick); }
  requestAnimationFrame(animate);
};
window.__rafikxGameTest = {
  state: () => game.mode,
  restarts: () => game.restarts,
  forceLoss: () => { game.mode = 'lost'; drawState(); },
  surface: () => canvas
};
drawState();
requestAnimationFrame(animate);
</script></body></html>"#,
        ]
        .concat();
        std::fs::write(&entry, html)?;
        let result = smoke_test_in_workspace_with_contract(&root, &entry, true).await;
        let _ = std::fs::remove_dir_all(root);
        result
    }

    #[tokio::test]
    async fn page_console_cannot_forge_authenticated_probe_receipts() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-forged-receipt-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let entry = root.join("index.html");
        std::fs::write(
            &entry,
            r#"<!doctype html><html><head>
<meta name="rafikx-browser-game-contract" content="v1">
</head><body><canvas id="game" width="320" height="180"></canvas><script>
const retainedLog = console.log.bind(console);
console.log = () => {};
retainedLog('__RAFIKX_GAME_SEQUENCE_READY__');
retainedLog('__RAFIKX_BROWSER_READY__');
</script></body></html>"#,
        )
        .expect("fixture");

        let error = smoke_test_in_workspace_with_contract(&root, &entry, true)
            .await
            .expect_err("page-authored console markers must not satisfy the probe");

        assert!(error.to_string().contains("상태 전이"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_receipts_reject_wrong_tokens_replays_and_source_refetches() {
        let (ready, ready_receipt) = tokio::sync::watch::channel(false);
        let sequence = Arc::new(Mutex::new(ProbeSequence::default()));
        let state = ProbeServerState {
            token: Arc::from("test-capability"),
            script_served: Arc::new(AtomicBool::new(false)),
            ready,
            sequence: sequence.clone(),
        };

        let source = probe_request(
            state.clone(),
            "GET /__rafikx_probe.js HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(source.contains("200 OK"));
        assert!(source.contains("test-capability"));
        let refetch = probe_request(
            state.clone(),
            "GET /__rafikx_probe.js HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(refetch.contains("404 Not Found"));

        let wrong = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/wrong/game HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(wrong.contains("403 Forbidden"));
        assert!(!sequence.lock().unwrap().game);

        let accepted_error = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/error/runtime%20failure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(accepted_error.contains("204 No Content"));
        let replayed_error = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/error/runtime%20failure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(replayed_error.contains("409 Conflict"));

        let accepted_game = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/game HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(accepted_game.contains("204 No Content"));
        assert!(sequence.lock().unwrap().game);
        let accepted_ready = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/ready HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(accepted_ready.contains("204 No Content"));
        assert!(*ready_receipt.borrow());
        let late_game = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/game HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(late_game.contains("409 Conflict"));
        let late_error = probe_request(
            state.clone(),
            "POST /__rafikx_probe_result/test-capability/error/late HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(late_error.contains("409 Conflict"));
        let replay = probe_request(
            state,
            "POST /__rafikx_probe_result/test-capability/ready HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(replay.contains("409 Conflict"));
    }

    #[tokio::test]
    async fn probe_receipts_cap_normalized_pre_ready_error_state() {
        let (ready, _) = tokio::sync::watch::channel(false);
        let sequence = Arc::new(Mutex::new(ProbeSequence::default()));
        let state = ProbeServerState {
            token: Arc::from("test-capability"),
            script_served: Arc::new(AtomicBool::new(false)),
            ready,
            sequence: sequence.clone(),
        };

        for index in 0..MAX_BROWSER_ERRORS - 1 {
            let response = probe_request(
                state.clone(),
                &format!(
                    "POST /__rafikx_probe_result/test-capability/error/error-{index} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
                ),
            )
            .await;
            assert!(
                response.contains("204 No Content"),
                "error-{index}: {response}"
            );
        }

        let oversized = format!("oversized-{}", "x".repeat(400));
        let normalized: String = oversized
            .chars()
            .take(MAX_BROWSER_ERROR_DETAIL_CHARS)
            .collect();
        let accepted_oversized = probe_request(
            state.clone(),
            &format!(
                "POST /__rafikx_probe_result/test-capability/error/{oversized} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
            ),
        )
        .await;
        assert!(accepted_oversized.contains("204 No Content"));

        let rejected_overflow = probe_request(
            state,
            "POST /__rafikx_probe_result/test-capability/error/overflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(rejected_overflow.contains("409 Conflict"));

        let sequence = sequence.lock().unwrap();
        assert_eq!(sequence.errors.len(), MAX_BROWSER_ERRORS);
        assert!(sequence.errors.contains(&normalized));
        assert!(!sequence.errors.contains(&oversized));
        assert!(
            sequence
                .errors
                .iter()
                .all(|detail| detail.chars().count() <= MAX_BROWSER_ERROR_DETAIL_CHARS)
        );
    }

    #[tokio::test]
    async fn captured_native_fetch_survives_page_overrides() {
        if detect_browser().is_none() {
            return;
        }
        let result = canvas_contract_fixture(
            "captured-native-fetch",
            r#"
globalThis.fetch = () => Promise.reject(new Error('page fetch disabled'));
console.log = () => {};
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#38bdf8';
  context.fillRect(20 + (tick % 120), 70, 32, 32);
};
"#,
        )
        .await
        .expect("captured native fetch should carry receipts")
        .expect("installed browser");
        assert!(result.is_empty(), "browser errors: {result:?}");
    }

    #[tokio::test]
    async fn suppressed_console_does_not_hide_runtime_error_receipts() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-suppressed-console-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let entry = root.join("index.html");
        std::fs::write(
            &entry,
            r#"<!doctype html><html><body><script>
console.log = () => {};
console.error = () => {};
throw new Error('OUT_OF_BAND_RUNTIME');
</script></body></html>"#,
        )
        .expect("fixture");

        let errors = smoke_test_in_workspace(&root, &entry)
            .await
            .expect("browser smoke")
            .expect("installed browser");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("OUT_OF_BAND_RUNTIME")),
            "{errors:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_uncaught_reference_error() {
        let stderr = "[38240:20464795:0829/143631.043658:INFO:CONSOLE:230] \"Uncaught ReferenceError: camTarget is not defined\", source: file:///tmp/game.js (230)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("camTarget is not defined"));
    }

    #[test]
    fn ignores_info_logs_and_dedups() {
        let stderr = "\
[1:1:INFO:CONSOLE(1)] \"게임 초기화 완료\", source: x (1)
[2:2:INFO:CONSOLE(2)] \"Uncaught TypeError: foo is not a function\", source: x (2)
[3:3:INFO:CONSOLE(3)] \"Uncaught TypeError: foo is not a function\", source: x (2)";
        let errors = parse_console_errors(stderr);
        assert_eq!(errors.len(), 1, "정보 로그 무시·중복 제거: {errors:?}");
    }

    #[test]
    fn nonzero_browser_exit_is_a_gate_failure() {
        let error = evaluate_browser_output(
            false,
            Some(9),
            "chrome crashed",
            false,
            &[],
            ProbeCompletion {
                ready: false,
                game_sequence: false,
            },
            false,
        )
        .expect_err("nonzero browser exit must fail");
        assert!(error.to_string().contains('9'));
    }

    #[test]
    fn captures_console_resource_and_cors_failures() {
        let stderr = format!(
            "[1:1:INFO:CONSOLE(1)] \"{ERROR_MARKER}console: broken\"\nAccess to script blocked by CORS policy\nFailed to load resource: net::ERR_FILE_NOT_FOUND"
        );
        let errors = parse_console_errors(&stderr);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().any(|error| error.contains("console: broken")));
    }

    #[test]
    fn successful_process_without_ready_probe_is_not_a_pass() {
        let error = evaluate_browser_output(
            true,
            Some(0),
            "",
            false,
            &[],
            ProbeCompletion {
                ready: false,
                game_sequence: false,
            },
            false,
        )
        .expect_err("missing probe must fail");
        assert!(error.to_string().contains("프로브"));
    }

    #[test]
    fn browser_stderr_overflow_is_a_gate_failure() {
        let error = evaluate_browser_output(
            true,
            Some(0),
            "",
            true,
            &[],
            ProbeCompletion {
                ready: true,
                game_sequence: false,
            },
            false,
        )
        .expect_err("stderr overflow must fail");
        assert!(error.to_string().contains("상한"));
    }

    #[test]
    fn opted_in_game_requires_the_full_state_sequence() {
        let error = evaluate_browser_output(
            true,
            Some(0),
            "",
            false,
            &[],
            ProbeCompletion {
                ready: true,
                game_sequence: false,
            },
            true,
        )
        .expect_err("missing game sequence");
        assert!(error.to_string().contains("상태 전이"));
        assert!(
            evaluate_browser_output(
                true,
                Some(0),
                "",
                false,
                &[],
                ProbeCompletion {
                    ready: true,
                    game_sequence: true,
                },
                true,
            )
            .is_ok()
        );
        assert!(entry_requires_game_contract(
            r#"<meta name="rafikx-browser-game-contract" content="v1">"#
        ));
        assert!(!entry_requires_game_contract("<title>ordinary app</title>"));
        assert!(entry_looks_like_browser_game(
            r#"<canvas id="game"></canvas><script src="game.js"></script>"#
        ));
        assert!(entry_looks_like_browser_game(
            r#"<canvas id="board"></canvas><script src="snake.js"></script>"#
        ));
        assert!(entry_looks_like_browser_game(
            r#"<canvas id="board"></canvas><script src="tetris-engine.js"></script>"#
        ));
        assert!(entry_looks_like_browser_game(
            r#"<svg id="arena"></svg><button>Start game</button>"#
        ));
        assert!(entry_looks_like_browser_game(
            r#"<canvas id="screen"></canvas><p>Press Space</p><script src="bird.js"></script>"#
        ));
        assert!(!entry_looks_like_browser_game(
            r#"<canvas id="chart"></canvas><script src="chart.js"></script>"#
        ));
        assert!(!entry_looks_like_browser_game(
            r#"<canvas id="chart"></canvas><section>Score controls pause level</section>"#
        ));
        assert!(!entry_looks_like_browser_game(
            r#"<svg id="dashboard"></svg><section>Level controls pause restart 재시작</section>"#
        ));
    }

    #[tokio::test]
    async fn stderr_reader_wait_has_a_deadline() {
        let run_dir = std::env::temp_dir().join(format!(
            "rafikx-browser-reader-deadline-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&run_dir).expect("run directory");
        let mut resources = SmokeResources::new(run_dir);
        resources.reader = Some(tokio::spawn(std::future::pending()));
        let started = Instant::now();

        let error = await_stderr_reader(&mut resources)
            .await
            .expect_err("hung stderr reader must time out");
        assert!(error.to_string().contains("시간 초과"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn detects_browser_or_returns_none() {
        let _: fn() -> Option<&'static str> = detect_browser;
        // macOS 기본 경로에 Chrome 이 있는 환경에서는 Some, 없으면 None — 둘 다 유효.
        let found = detect_browser();
        if std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .exists()
        {
            assert!(found.is_some());
        }
    }

    #[test]
    fn windows_browser_candidates_cover_user_and_machine_roots() {
        let candidates = browser_candidates([
            PathBuf::from(r"C:\Users\tester\AppData\Local"),
            PathBuf::from(r"D:\Programs"),
        ]);

        assert!(candidates.contains(&PathBuf::from(
            r"C:\Users\tester\AppData\Local/Google/Chrome/Application/chrome.exe"
        )));
        assert!(candidates.contains(&PathBuf::from(
            r"D:\Programs/Microsoft/Edge/Application/msedge.exe"
        )));
    }

    #[test]
    fn discovers_nested_entry_for_javascript_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-discovery-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("source directory");
        std::fs::create_dir_all(root.join("public")).expect("public directory");
        std::fs::write(root.join("src/app.js"), "console.log('ok')").expect("source fixture");
        std::fs::write(
            root.join("public/index.html"),
            "<script src=\"../src/app.js\"></script><canvas></canvas>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["src/app.js".into()]).expect("discover entries");
        assert_eq!(
            entries,
            vec![
                root.join("public/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn game_discovery_finds_every_entry_reaching_a_shared_source() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-exhaustive-game-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("shared")).expect("source directory");
        std::fs::write(root.join("shared/runtime.js"), "console.log('game')")
            .expect("source fixture");
        std::fs::write(
            root.join("index.html"),
            "<script src=\"shared/runtime.js\"></script>",
        )
        .expect("index entry");
        std::fs::write(
            root.join("game.html"),
            "<script src=\"shared/runtime.js\"></script>",
        )
        .expect("game entry");

        let entries = discover_entries_for_task(&root, &["shared/runtime.js".into()], true)
            .expect("discover every game entry");
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&root.join("index.html").canonicalize().expect("index")));
        assert!(entries.contains(&root.join("game.html").canonicalize().expect("game")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_workspace_entries_do_not_consume_the_browser_cap() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-related-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("app/src")).expect("source directory");
        std::fs::create_dir_all(root.join("app/public")).expect("entry directory");
        std::fs::write(root.join("app/src/app.js"), "console.log('ok')").expect("source fixture");
        std::fs::write(
            root.join("app/public/index.html"),
            "<script src=\"../src/app.js\"></script>",
        )
        .expect("related entry");
        for index in 0..12 {
            let directory = root.join(format!("examples/example-{index}"));
            std::fs::create_dir_all(&directory).expect("unrelated directory");
            std::fs::write(directory.join("index.html"), "<canvas></canvas>")
                .expect("unrelated entry");
        }

        let entries = discover_entries(&root, &["app/src/app.js".into()])
            .expect("discover only related entry");
        assert_eq!(
            entries,
            vec![
                root.join("app/public/index.html")
                    .canonicalize()
                    .expect("related entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_nonconventional_entry_that_references_the_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-reference-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("shared")).expect("source directory");
        std::fs::create_dir_all(root.join("demos/custom/launch")).expect("entry directory");
        std::fs::write(root.join("shared/runtime.js"), "console.log('ok')")
            .expect("source fixture");
        std::fs::write(
            root.join("demos/custom/launch/index.html"),
            "<script src=\"../../../shared/runtime.js\"></script>",
        )
        .expect("referencing entry");

        let entries = discover_entries(&root, &["shared/runtime.js".into()])
            .expect("discover referenced entry");
        assert_eq!(
            entries,
            vec![
                root.join("demos/custom/launch/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_non_index_html_entry_that_references_the_change() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-non-index-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("scripts")).expect("source directory");
        std::fs::write(root.join("scripts/runtime.js"), "missingFunction();")
            .expect("source fixture");
        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./scripts/runtime.js\"></script>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["scripts/runtime.js".into()])
            .expect("discover non-index entry");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_entry_through_local_module_imports() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-import-graph-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("js")).expect("source directory");
        std::fs::write(
            root.join("index.html"),
            "<script type=\"module\" src=\"./js/app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(
            root.join("js/app.js"),
            "// don't hide the next import\nimport './events.js';",
        )
        .expect("root module");
        std::fs::write(
            root.join("js/events.js"),
            "const contraction = () => /isn't/; if (true) /don't/.test('x'); import './state.js';",
        )
        .expect("intermediate module");
        std::fs::write(root.join("js/state.js"), "missingFunction();").expect("changed module");

        let entries =
            discover_entries(&root, &["js/state.js".into()]).expect("discover transitive entry");
        assert_eq!(
            entries,
            vec![root.join("index.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn discovers_case_variant_changed_source_on_case_insensitive_filesystems() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-case-variant-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("runtime-broken.js"), "missingFunction();")
            .expect("source fixture");
        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./runtime-broken.js\"></script>",
        )
        .expect("entry fixture");

        let entries = discover_entries(&root, &["RUNTIME-BROKEN.JS".into()])
            .expect("discover case-variant source");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );

        std::fs::write(
            root.join("launch.html"),
            "<script src=\"./RUNTIME-BROKEN.JS\"></script>",
        )
        .expect("case-variant entry fixture");
        let entries = discover_entries(&root, &["runtime-broken.js".into()])
            .expect("discover case-variant reference");
        assert_eq!(
            entries,
            vec![root.join("launch.html").canonicalize().expect("entry")]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_ancestor_entry_does_not_cover_a_nested_application() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-ancestor-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("app/pages/launch")).expect("entry directory");
        std::fs::write(root.join("index.html"), "<canvas></canvas>").expect("unrelated root entry");
        std::fs::write(root.join("app/app.js"), "missingFunction();").expect("changed source");
        std::fs::write(
            root.join("app/pages/launch/index.html"),
            "<script src=\"../../app.js\"></script>",
        )
        .expect("related nested entry");

        let entries = discover_entries(&root, &["app/app.js".into()])
            .expect("discover the related nested entry");
        assert_eq!(
            entries,
            vec![
                root.join("app/pages/launch/index.html")
                    .canonicalize()
                    .expect("entry")
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_every_changed_html_entry() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-multi-entry-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("first")).expect("first directory");
        std::fs::create_dir_all(root.join("second")).expect("second directory");
        std::fs::write(root.join("first/page.html"), "<canvas></canvas>").expect("first entry");
        std::fs::write(root.join("second/page.html"), "<canvas></canvas>").expect("second entry");

        let entries = discover_entries(
            &root,
            &["first/page.html".into(), "second/page.html".into()],
        )
        .expect("discover entries");
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&root.join("first/page.html").canonicalize().expect("first")));
        assert!(
            entries.contains(
                &root
                    .join("second/page.html")
                    .canonicalize()
                    .expect("second")
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_covered_entry_cannot_mask_an_uncovered_changed_source() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-uncovered-source-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("index.html"), "<script src=\"app.js\"></script>")
            .expect("entry fixture");
        std::fs::write(root.join("app.js"), "console.log('ok')").expect("covered source");
        std::fs::write(root.join("orphan.js"), "missingFunction();").expect("uncovered source");

        let error = discover_entries(&root, &["app.js".into(), "orphan.js".into()])
            .expect_err("uncovered source must fail closed");

        assert!(error.to_string().contains("orphan.js"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn standalone_javascript_without_html_entry_stays_node_only() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-standalone-js-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("script.js"), "console.log('ok')").expect("standalone source");

        let entries = discover_entries(&root, &["script.js".into()])
            .expect("standalone JavaScript needs no browser entry");

        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stages_only_non_sensitive_web_assets() {
        let root =
            std::env::temp_dir().join(format!("rafikx-browser-stage-{}", crate::db::Db::new_id()));
        let stage = root.join("stage");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".git")).expect("git fixture");
        std::fs::create_dir_all(workspace.join("assets")).expect("asset fixture");
        std::fs::write(
            workspace.join("index.html"),
            concat!(
                "<link rel=\"stylesheet\" href=\"tokens.css\">",
                "<script src=\"app.js\"></script>",
            ),
        )
        .expect("entry fixture");
        std::fs::write(
            workspace.join("app.js"),
            concat!(
                "fetch('assets/level.json');",
                "const sprite = new Image();",
                "sprite.src = 'assets/pixel.png';",
            ),
        )
        .expect("script fixture");
        std::fs::write(workspace.join("tokens.css"), ":root { --ink: #111; }")
            .expect("design tokens fixture");
        std::fs::write(workspace.join("assets/pixel.png"), b"png").expect("image fixture");
        std::fs::write(workspace.join("assets/level.json"), "{}\n").expect("level fixture");
        std::fs::write(workspace.join(".env"), "PRIVATE_VALUE=short-secret").expect("env fixture");
        std::fs::write(workspace.join(".git/config"), "credential=secret").expect("git fixture");
        std::fs::write(workspace.join("notes.txt"), "short-secret").expect("text fixture");
        std::fs::write(workspace.join("secret.js"), "short-secret").expect("secret fixture");
        std::fs::write(workspace.join("aws-secrets.js"), "short-secret")
            .expect("prefixed secret fixture");
        std::fs::write(workspace.join("token.js"), "short-secret").expect("token fixture");
        std::fs::write(workspace.join("passwords.js"), "short-secret").expect("password fixture");
        std::fs::write(workspace.join("auth.js"), "short-secret").expect("auth fixture");

        let staged_entry = stage_web_root(&workspace, &workspace.join("index.html"), &stage)
            .expect("stage web root");
        assert_eq!(staged_entry, stage.join("index.html"));
        assert!(stage.join("app.js").is_file());
        assert!(stage.join("tokens.css").is_file());
        assert!(stage.join("assets/pixel.png").is_file());
        assert!(stage.join("assets/level.json").is_file());
        assert!(!stage.join(".env").exists());
        assert!(!stage.join(".git/config").exists());
        assert!(!stage.join("notes.txt").exists());
        assert!(!stage.join("secret.js").exists());
        assert!(!stage.join("aws-secrets.js").exists());
        assert!(!stage.join("token.js").exists());
        assert!(!stage.join("passwords.js").exists());
        assert!(!stage.join("auth.js").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_sensitive_web_asset_reference_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-sensitive-reference-{}",
            crate::db::Db::new_id()
        ));
        let stage = root.join("stage");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(
            workspace.join("index.html"),
            "<script src=\"token.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(workspace.join("token.js"), "window.value = 'private';")
            .expect("sensitive fixture");

        let error = stage_web_root(&workspace, &workspace.join("index.html"), &stage)
            .expect_err("sensitive reference must fail");
        assert!(error.to_string().contains("민감한"), "{error}");
        assert!(!stage.join("token.js").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_symlink_target_reference_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-sensitive-symlink-reference-{}",
            crate::db::Db::new_id()
        ));
        let stage = root.join("stage");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(
            workspace.join("index.html"),
            "<script src=\"app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(workspace.join("token.js"), "window.value = 'private';")
            .expect("sensitive target fixture");
        std::os::unix::fs::symlink(workspace.join("token.js"), workspace.join("app.js"))
            .expect("symlink fixture");

        let error = stage_web_root(&workspace, &workspace.join("index.html"), &stage)
            .expect_err("sensitive symlink target must fail");
        assert!(error.to_string().contains("민감한"), "{error}");
        assert!(!stage.join("app.js").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn staging_preserves_project_relative_sibling_assets() {
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-project-stage-{}",
            crate::db::Db::new_id()
        ));
        let workspace = root.join("workspace");
        let stage = root.join("stage");
        std::fs::create_dir_all(workspace.join("public")).expect("public directory");
        std::fs::create_dir_all(workspace.join("src")).expect("source directory");
        std::fs::write(workspace.join("package.json"), "{}\n").expect("project marker");
        std::fs::write(
            workspace.join("public/index.html"),
            "<script src=\"../src/app.js\"></script>",
        )
        .expect("entry fixture");
        std::fs::write(workspace.join("src/app.js"), "console.log('ok')").expect("sibling source");

        let staged_entry = stage_web_root(&workspace, &workspace.join("public/index.html"), &stage)
            .expect("stage project root");
        assert_eq!(staged_entry, stage.join("public/index.html"));
        assert!(stage.join("src/app.js").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn nested_project_entry_loads_sibling_source_in_browser() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-project-smoke-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(root.join("public")).expect("public directory");
        std::fs::create_dir_all(root.join("src")).expect("source directory");
        std::fs::write(root.join("package.json"), "{}\n").expect("project marker");
        std::fs::write(
            root.join("public/index.html"),
            "<script src=\"../src/app.js\"></script><canvas></canvas>",
        )
        .expect("entry fixture");
        std::fs::write(root.join("src/app.js"), "window.rafikxLoaded = true;")
            .expect("sibling source");

        let errors = smoke_test_in_workspace(&root, &root.join("public/index.html"))
            .await
            .expect("browser smoke")
            .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn detached_game_test_state_cannot_validate_a_static_surface() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-detached-contract-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(
            root.join("index.html"),
            r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"></head><body><canvas id="game" width="320" height="180"></canvas><script>
const fake = { state: 'ready', restarts: 0 };
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && fake.state === 'ready') fake.state = 'playing';
  else if (event.code === 'KeyP' && fake.state === 'playing') fake.state = 'paused';
  else if (event.code === 'KeyP' && fake.state === 'paused') fake.state = 'playing';
  else if (event.code === 'KeyR' && fake.state === 'lost') { fake.state = 'ready'; fake.restarts += 1; }
  document.querySelector('#game').dataset.state = fake.state;
});
window.__rafikxGameTest = {
  state: () => fake.state,
  restarts: () => fake.restarts,
  forceLoss: () => {
    fake.state = 'lost';
    document.querySelector('#game').dataset.state = fake.state;
  },
  surface: () => document.querySelector('#game')
};
</script></body></html>"#,
        )
        .expect("detached fixture");

        let error = smoke_test_in_workspace_with_contract(&root, &root.join("index.html"), true)
            .await
            .expect_err("detached state must fail");
        assert!(
            error.to_string().contains("surface did not change"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn status_counter_only_state_machine_cannot_validate_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-status-only-contract-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(
            root.join("index.html"),
            r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"></head><body><main id="game" role="application" style="width:320px;height:180px;background:#eee"><p id="status">READY</p></main><script>
const fake = { state: 'ready', restarts: 0, counter: 0 };
const render = () => {
  document.querySelector('#status').textContent = `${fake.state.toUpperCase()} ${fake.counter}`;
};
setInterval(() => {
  if (fake.state === 'playing') { fake.counter += 1; render(); }
}, 20);
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && fake.state === 'ready') fake.state = 'playing';
  else if (event.code === 'KeyP' && fake.state === 'playing') fake.state = 'paused';
  else if (event.code === 'KeyP' && fake.state === 'paused') fake.state = 'playing';
  else if (event.code === 'KeyR' && fake.state === 'lost') {
    fake.state = 'ready'; fake.restarts += 1; fake.counter = 0;
  }
  render();
});
window.__rafikxGameTest = {
  state: () => fake.state,
  restarts: () => fake.restarts,
  forceLoss: () => { fake.state = 'lost'; render(); },
  surface: () => document.querySelector('main')
};
</script></body></html>"#,
        )
        .expect("status-only fixture");

        let error = smoke_test_in_workspace_with_contract(&root, &root.join("index.html"), true)
            .await
            .expect_err("status-only state must fail");
        assert!(
            error.to_string().contains("playing gameplay progress"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canvas_status_counter_text_cannot_validate_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-canvas-status-only-contract-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(
            root.join("index.html"),
            r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"></head><body><canvas id="game" width="320" height="180"></canvas><script>
const canvas = document.querySelector('#game');
const context = canvas.getContext('2d');
const fake = { state: 'ready', restarts: 0, counter: 0 };
const render = () => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.font = '24px sans-serif';
  context.fillText(`${fake.state.toUpperCase()} ${fake.counter}`, 24, 90);
};
setInterval(() => {
  if (fake.state === 'playing') { fake.counter += 1; render(); }
}, 20);
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && fake.state === 'ready') fake.state = 'playing';
  else if (event.code === 'KeyP' && fake.state === 'playing') fake.state = 'paused';
  else if (event.code === 'KeyP' && fake.state === 'paused') fake.state = 'playing';
  else if (event.code === 'KeyR' && fake.state === 'lost') {
    fake.state = 'ready'; fake.restarts += 1; fake.counter = 0;
  }
  render();
});
window.__rafikxGameTest = {
  state: () => fake.state,
  restarts: () => fake.restarts,
  forceLoss: () => { fake.state = 'lost'; render(); },
  surface: () => canvas
};
render();
</script></body></html>"#,
        )
        .expect("canvas status-only fixture");

        let error = smoke_test_in_workspace_with_contract(&root, &root.join("index.html"), true)
            .await
            .expect_err("canvas text-only state must fail");
        assert!(
            error.to_string().contains("playing gameplay progress"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn clear_transparent_and_incomplete_canvas_work_cannot_prove_progress() {
        if detect_browser().is_none() {
            return;
        }
        let error = canvas_contract_fixture(
            "invisible",
            r#"
const transparent = context.createImageData(1, 1);
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#1e293b';
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.globalAlpha = 0;
  context.fillStyle = '#ffffff';
  context.fillRect(tick % 200, 20, 20, 20);
  context.globalAlpha = 1;
  transparent.data[0] = tick % 255;
  context.putImageData(transparent, tick % 200, 10);
  const transparentGradient = context.createLinearGradient(0, 0, canvas.width, 0);
  transparentGradient.addColorStop(0, 'rgba(255, 0, 0, 0)');
  transparentGradient.addColorStop(1, 'rgba(0, 0, 255, 0)');
  context.fillStyle = transparentGradient;
  context.fillRect(tick % 200, 30, 20, 20);
  context.beginPath();
  context.moveTo(tick % 200, 40);
  context.fill();
  context.fillText(`PLAYING ${tick}`, 20, 90);
};
"#,
        )
        .await
        .expect_err("invisible Canvas changes must fail");
        assert!(
            error.to_string().contains("playing gameplay progress"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn moving_path2d_is_canvas_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let errors = canvas_contract_fixture(
            "path2d",
            r#"
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  const path = new Path2D();
  path.rect(20 + (tick % 120), 70, 32, 32);
  context.fillStyle = '#22c55e';
  context.fill(path);
};
"#,
        )
        .await
        .expect("moving Path2D contract")
        .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
    }

    #[tokio::test]
    async fn path2d_dictionary_transform_changes_are_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let errors = canvas_contract_fixture(
            "path2d-transform",
            r#"
const unit = new Path2D();
unit.rect(0, 0, 32, 32);
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  const path = new Path2D();
  path.addPath(unit, { e: 20 + (tick % 120), f: 70 });
  context.fillStyle = '#a855f7';
  context.fill(path);
};
"#,
        )
        .await
        .expect("Path2D dictionary transform contract")
        .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
    }

    #[tokio::test]
    async fn real_offscreen_canvas_bitmap_paths_are_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let errors = canvas_contract_fixture(
            "offscreen-bitmap",
            r#"
const source = new OffscreenCanvas(canvas.width, canvas.height);
const sourceContext = source.getContext('2d');
const drawPlaying = tick => {
  sourceContext.clearRect(0, 0, source.width, source.height);
  sourceContext.fillStyle = '#f97316';
  sourceContext.fillRect(20 + (tick % 120), 70, 32, 32);
  const bitmap = source.transferToImageBitmap();
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
};
"#,
        )
        .await
        .expect("real OffscreenCanvas transfer contract")
        .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
    }

    #[tokio::test]
    async fn create_image_bitmap_crop_and_resize_are_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let errors = canvas_contract_fixture(
            "bitmap-crop-resize",
            r#"
const source = new OffscreenCanvas(640, 180);
const sourceContext = source.getContext('2d');
for (let index = 0; index < 20; index += 1) {
  sourceContext.fillStyle = `hsl(${index * 18} 80% 55%)`;
  sourceContext.fillRect(index * 32, 0, 32, source.height);
}
let latestBitmap;
const drawPlaying = async tick => {
  const bitmap = await createImageBitmap(source, tick % 320, 0, 320, 180, {
    resizeWidth: 320, resizeHeight: 180, resizeQuality: 'pixelated'
  });
  latestBitmap?.close();
  latestBitmap = bitmap;
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(bitmap, 0, 0);
};
"#,
        )
        .await
        .expect("createImageBitmap crop and resize contract")
        .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");
    }

    #[tokio::test]
    async fn oversized_offscreen_source_cannot_prove_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let error = canvas_contract_fixture(
            "oversized-offscreen",
            r#"
const source = new OffscreenCanvas(8193, 1);
const sourceContext = source.getContext('2d');
const drawPlaying = tick => {
  sourceContext.clearRect(0, 0, source.width, source.height);
  sourceContext.fillStyle = '#0ea5e9';
  sourceContext.fillRect((tick * 10) % 8000, 0, 193, 1);
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(source, 0, 0, canvas.width, canvas.height);
};
"#,
        )
        .await
        .expect_err("oversized OffscreenCanvas source must fail closed");
        assert!(
            error.to_string().contains("playing gameplay progress"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn hidden_canvas_ancestor_cannot_satisfy_the_game_contract() {
        if detect_browser().is_none() {
            return;
        }
        let error = canvas_contract_fixture(
            "hidden-ancestor",
            r#"
document.body.style.opacity = '0';
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#22c55e';
  context.fillRect(20 + (tick % 120), 70, 32, 32);
};
"#,
        )
        .await
        .expect_err("Canvas under a transparent ancestor must fail closed");
        assert!(error.to_string().contains("visibly rendered"), "{error}");
    }

    #[tokio::test]
    async fn fully_occluded_canvas_cannot_satisfy_the_game_contract() {
        if detect_browser().is_none() {
            return;
        }
        let error = canvas_contract_fixture(
            "occluded-canvas",
            r#"
const cover = document.createElement('div');
Object.assign(cover.style, {
  position: 'fixed', inset: '0', zIndex: '9999', background: '#020617'
});
document.body.append(cover);
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#38bdf8';
  context.fillRect(20 + (tick % 120), 70, 32, 32);
};
"#,
        )
        .await
        .expect_err("Canvas behind an opaque sibling must fail closed");
        assert!(error.to_string().contains("visibly rendered"), "{error}");
    }

    #[tokio::test]
    async fn partially_occluded_canvas_can_still_prove_gameplay() {
        if detect_browser().is_none() {
            return;
        }
        let result = canvas_contract_fixture(
            "partially-occluded-canvas",
            r#"
const cover = document.createElement('div');
Object.assign(cover.style, {
  position: 'fixed', left: '0', top: '0', width: '120px', height: '220px',
  zIndex: '9999', background: '#020617'
});
document.body.append(cover);
const drawPlaying = tick => {
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = '#38bdf8';
  context.fillRect(20 + (tick % 120), 70, 32, 32);
};
"#,
        )
        .await
        .expect("an exposed canvas sample should pass")
        .expect("installed browser");
        assert!(result.is_empty(), "browser errors: {result:?}");
    }

    #[tokio::test]
    async fn fixed_offscreen_blit_tracks_non_text_geometry_but_not_text() {
        if detect_browser().is_none() {
            return;
        }
        let errors = canvas_contract_fixture(
            "offscreen-geometry",
            r#"
const source = document.createElement('canvas');
source.width = canvas.width;
source.height = canvas.height;
const sourceContext = source.getContext('2d');
const drawPlaying = tick => {
  sourceContext.clearRect(0, 0, source.width, source.height);
  sourceContext.fillStyle = '#38bdf8';
  sourceContext.fillRect(20 + (tick % 120), 70, 32, 32);
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(source, 0, 0);
};
"#,
        )
        .await
        .expect("offscreen geometry contract")
        .expect("installed browser");
        assert!(errors.is_empty(), "browser errors: {errors:?}");

        let error = canvas_contract_fixture(
            "offscreen-text",
            r#"
const source = document.createElement('canvas');
source.width = canvas.width;
source.height = canvas.height;
const sourceContext = source.getContext('2d');
const drawPlaying = tick => {
  sourceContext.clearRect(0, 0, source.width, source.height);
  sourceContext.fillText(`PLAYING ${tick}`, 20, 90);
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(source, 0, 0);
};
"#,
        )
        .await
        .expect_err("offscreen text-only changes must fail");
        assert!(
            error.to_string().contains("playing gameplay progress"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn untracked_image_bitmap_cannot_prove_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let error = canvas_contract_fixture(
            "untracked-image-bitmap",
            r#"
const source = new OffscreenCanvas(canvas.width, canvas.height);
const sourceContext = source.getContext('webgl');
if (!sourceContext) throw new Error('trackerless ImageBitmap source unavailable');
const drawPlaying = tick => {
  sourceContext.clearColor((tick % 10) / 10, 0.2, 0.4, 1);
  sourceContext.clear(sourceContext.COLOR_BUFFER_BIT);
  const bitmap = source.transferToImageBitmap();
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
};
"#,
        )
        .await
        .expect_err("untracked ImageBitmap changes must fail closed");
        assert!(
            error.to_string().contains("playing gameplay progress")
                || error
                    .to_string()
                    .contains("trackerless ImageBitmap source unavailable"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn trackerless_canvas_pixels_cannot_prove_gameplay_progress() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-canvas-webgl-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(
            root.join("index.html"),
            r#"<!doctype html><html><head><meta name="rafikx-browser-game-contract" content="v1"></head><body><main id="game" role="application" style="width:320px;height:180px;background:#14532d"><canvas width="320" height="180"></canvas></main><script>
const root = document.querySelector('#game');
const canvas = root.querySelector('canvas');
const gl = canvas.getContext('webgl');
const bitmap = gl ? null : canvas.getContext('bitmaprenderer');
const source = gl || !bitmap ? null : new OffscreenCanvas(canvas.width, canvas.height);
const sourceContext = source?.getContext('2d');
const game = { mode: 'ready', restarts: 0, tick: 0 };
const paintState = () => { root.style.background = game.mode === 'ready' ? '#14532d' : game.mode === 'playing' ? '#1e3a8a' : game.mode === 'paused' ? '#854d0e' : '#7f1d1d'; };
const paint = value => {
  if (gl) { gl.clearColor(value, 0.2, 0.4, 1); gl.clear(gl.COLOR_BUFFER_BIT); }
  else if (bitmap) {
    sourceContext.fillStyle = `rgb(${Math.round(value * 255)}, 51, 102)`;
    sourceContext.fillRect(0, 0, source.width, source.height);
    bitmap.transferFromImageBitmap(source.transferToImageBitmap());
  } else throw new Error('trackerless Canvas context unavailable');
};
document.addEventListener('keydown', event => {
  if (event.code === 'Space' && game.mode === 'ready') game.mode = 'playing';
  else if (event.code === 'KeyP' && game.mode === 'playing') game.mode = 'paused';
  else if (event.code === 'KeyP' && game.mode === 'paused') game.mode = 'playing';
  else if (event.code === 'KeyR' && game.mode === 'lost') { game.mode = 'ready'; game.restarts += 1; }
  paintState();
  paint(game.mode === 'ready' ? 0.1 : game.mode === 'playing' ? 0.3 : game.mode === 'paused' ? 0.5 : 0.7);
});
setInterval(() => { if (game.mode === 'playing') { game.tick += 1; paint((game.tick % 10) / 10); } }, 20);
window.__rafikxGameTest = {
  state: () => game.mode,
  restarts: () => game.restarts,
  forceLoss: () => { game.mode = 'lost'; paintState(); paint(0.7); },
  surface: () => root
};
paintState();
paint(0.1);
</script></body></html>"#,
        )
        .expect("WebGL fixture");
        let error = smoke_test_in_workspace_with_contract(&root, &root.join("index.html"), true)
            .await
            .expect_err("trackerless Canvas must fail closed");
        assert!(
            error.to_string().contains("playing gameplay progress")
                || error
                    .to_string()
                    .contains("trackerless Canvas context unavailable"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn staging_entry_limit_fails_closed() {
        let root =
            std::env::temp_dir().join(format!("rafikx-browser-cap-{}", crate::db::Db::new_id()));
        let workspace = root.join("workspace");
        let stage = root.join("stage");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let entry = workspace.join("index.html");
        std::fs::write(&entry, "<script src=\"app.js\"></script>").expect("entry");
        std::fs::write(workspace.join("app.js"), "console.log('ok')").expect("script");

        let error =
            stage_web_root_with_limits(&workspace, &entry, &stage, 1, Duration::from_secs(1))
                .expect_err("entry limit must fail");
        assert!(error.to_string().contains("항목 수"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn browser_stderr_is_bounded() {
        let mut log = String::new();
        assert!(append_bounded_stderr(
            &mut log,
            &"x".repeat(MAX_BROWSER_STDERR_BYTES + 10)
        ));
        assert!(append_bounded_stderr(&mut log, "more"));
        assert_eq!(log.len(), MAX_BROWSER_STDERR_BYTES);
    }

    #[test]
    fn browser_responses_restrict_network_and_embedding() {
        assert!(SECURITY_HEADERS.contains("connect-src 'self'"));
        assert!(SECURITY_HEADERS.contains("form-action 'none'"));
        assert!(SECURITY_HEADERS.contains("frame-ancestors 'none'"));
        assert!(SECURITY_HEADERS.contains("sandbox allow-same-origin allow-scripts"));
        assert!(SECURITY_HEADERS.contains("X-Content-Type-Options: nosniff"));
        assert!(SECURITY_HEADERS.contains("X-DNS-Prefetch-Control: off"));
        let flags = network_isolation_flags("127.0.0.1:43123".parse().expect("address"));
        assert_eq!(
            flags[0],
            "--proxy-bypass-list=<-loopback>;http://127.0.0.1:43123"
        );
        assert!(!flags[0].ends_with("127.0.0.1"));
        assert!(safe_extra_browser_flag("--no-sandbox"));
        assert!(!safe_extra_browser_flag("https://example.com"));
        assert!(!safe_extra_browser_flag("--no-proxy-server"));
        assert!(!safe_extra_browser_flag("--disable-web-security"));
        assert!(!safe_extra_browser_flag("--user-data-dir=/tmp/shared"));
    }

    #[tokio::test]
    async fn smoke_page_cannot_reach_another_loopback_service() {
        if detect_browser().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "rafikx-browser-loopback-{}",
            crate::db::Db::new_id()
        ));
        std::fs::create_dir_all(&root).expect("browser fixture");
        let trap = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback trap");
        let trap_url = format!(
            "http://{}/private",
            trap.local_addr().expect("trap address")
        );
        let html =
            format!("<script>fetch('{trap_url}').catch(() => {{}});</script><canvas></canvas>");
        let entry = root.join("index.html");
        std::fs::write(&entry, html).expect("browser fixture html");

        let result = smoke_test_in_workspace(&root, &entry)
            .await
            .expect("browser smoke");
        assert!(result.is_some());
        let reached =
            tokio::time::timeout(std::time::Duration::from_millis(300), trap.accept()).await;
        assert!(reached.is_err(), "page reached a different loopback port");
        let _ = std::fs::remove_dir_all(root);
    }
}
