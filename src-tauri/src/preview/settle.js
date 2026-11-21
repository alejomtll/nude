// Runs only in the isolated capture WebView.
(() => {
  const rules = `*, *::before, *::after {
    animation-delay: 0s !important;
    animation-duration: 0.01s !important;
    transition-delay: 0s !important;
    transition-duration: 0.01s !important;
  }`;
  let styled = false;
  function applyStyle() {
    if (styled || !document.documentElement) return;
    const style = document.createElement("style");
    style.textContent = rules;
    (document.head || document.documentElement).appendChild(style);
    styled = true;
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", applyStyle, { once: true });
  } else {
    applyStyle();
  }

  // A hidden window can suspend animation frames. Deliver a timer fallback
  // once, preserving cancellation and avoiding duplicate callbacks.
  const requestFrame = window.requestAnimationFrame.bind(window);
  const cancelFrame = window.cancelAnimationFrame.bind(window);
  const frames = new Map();
  window.requestAnimationFrame = (callback) => {
    const run = (time) => {
      if (!frames.has(id)) return;
      window.clearTimeout(frames.get(id));
      frames.delete(id);
      callback(time);
    };
    const id = requestFrame(run);
    frames.set(id, window.setTimeout(() => {
      cancelFrame(id);
      run(performance.now());
    }, 32));
    return id;
  };
  window.cancelAnimationFrame = (id) => {
    window.clearTimeout(frames.get(id));
    frames.delete(id);
    cancelFrame(id);
  };

  const observers = new Map();
  const NativeObserver = window.IntersectionObserver;
  if (NativeObserver) {
    window.IntersectionObserver = class extends NativeObserver {
      constructor(callback, options) {
        const targets = new Map();
        super((entries, observer) => {
          for (const entry of entries) {
            if (targets.has(entry.target)) targets.set(entry.target, entry.isIntersecting);
          }
          callback.call(observer, entries, observer);
        }, options);
        this.targets = targets;
        this.deliverPreview = (entries) => callback.call(this, entries, this);
      }
      observe(target) {
        super.observe(target);
        if (!this.targets.has(target)) this.targets.set(target, false);
        observers.set(this, this.targets);
      }
      unobserve(target) {
        super.unobserve(target);
        this.targets.delete(target);
        if (!this.targets.size) observers.delete(this);
      }
      disconnect() {
        super.disconnect();
        this.targets.clear();
        observers.delete(this);
      }
    };
  }

  function signalVisibleTargets() {
    const viewport = new DOMRect(0, 0, window.innerWidth, window.innerHeight);
    for (const [observer, targets] of observers) {
      const root = observer.root instanceof Element ? observer.root.getBoundingClientRect() : viewport;
      const entries = [];
      for (const [target, visible] of targets) {
        if (visible || !target.isConnected) continue;
        const bounds = target.getBoundingClientRect();
        const x = Math.max(bounds.left, root.left, viewport.left);
        const y = Math.max(bounds.top, root.top, viewport.top);
        const width = Math.min(bounds.right, root.right, viewport.right) - x;
        const height = Math.min(bounds.bottom, root.bottom, viewport.bottom) - y;
        if (width <= 0 || height <= 0) continue;
        entries.push({
          target,
          time: performance.now(),
          rootBounds: root,
          boundingClientRect: bounds,
          intersectionRect: new DOMRect(x, y, width, height),
          intersectionRatio: width * height / (bounds.width * bounds.height),
          isIntersecting: true,
        });
        targets.set(target, true);
      }
      if (entries.length) {
        try { observer.deliverPreview(entries); } catch { /* This widget must not block the PNG. */ }
      }
    }
  }

  function finishAnimations() {
    for (const animation of document.getAnimations()) {
      if (!Number.isFinite(animation.effect?.getComputedTiming().endTime)) continue;
      try { animation.finish(); } catch { /* An idle animation may not have a timeline. */ }
    }
  }

  window.__nudePrepareCapture = () => {
    applyStyle();
    window.dispatchEvent(new Event("resize"));
    window.dispatchEvent(new Event("scroll"));
    signalVisibleTargets();
    finishAnimations();
  };
  window.__nudeFinishCaptureAnimations = finishAnimations;
})();
