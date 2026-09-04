import * as THREE from '../vendor/three.module.js'; // three vendored for offline CSP

const MAX_PARTICLES = 200_000;   // one draw call, GPU-instanced points

export class HoloCore {
  #lastFrame = performance.now();
  #slowFrames = 0;

  constructor(canvas) {
    this.canvas = canvas;
    this.renderer = new THREE.WebGLRenderer({
      canvas,
      antialias: false,
      powerPreference: 'high-performance',
      alpha: true,
    });
    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(55, window.innerWidth / window.innerHeight, 0.1, 100);
    this.camera.position.z = 7.5;
    this.camera.position.y = 1.2;
    this.camera.lookAt(0, 0, 0);

    this.quality = 1;             // auto-tier: 1 = full, 0.5 = halved
    this.#buildPointCloud();
    this.#bindLifecycle();
    this.#tick = this.#tick.bind(this);
    this.renderer.setAnimationLoop(this.#tick);
  }

  #buildPointCloud() {
    const geo = new THREE.BufferGeometry();
    const pos = new Float32Array(MAX_PARTICLES * 3);
    const state = new Float32Array(MAX_PARTICLES).fill(0); // all "lost" initially

    for (let i = 0; i < MAX_PARTICLES; i++) {
      // Deterministic lattice → looks like a disk platter spiral, not random noise
      const r = 2.4 * Math.sqrt(i / MAX_PARTICLES);
      const a = i * 2.399963;      // golden angle spiral
      pos[i * 3] = r * Math.cos(a);
      pos[i * 3 + 1] = (Math.sin(i * 0.05) * 0.15) + (Math.random() - 0.5) * 0.15;
      pos[i * 3 + 2] = r * Math.sin(a);
    }
    geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    geo.setAttribute('recoveryState', new THREE.BufferAttribute(state, 1));

    const mat = new THREE.ShaderMaterial({
      transparent: true,
      depthWrite: false,
      blending: THREE.AdditiveBlending,
      vertexShader: `
        attribute float recoveryState;
        varying vec3 vColor;
        void main() {
          vec3 lost     = vec3(0.28, 0.08, 0.14); // Deep Crimson Lost
          vec3 carved   = vec3(1.00, 0.62, 0.28); // Amber Carved
          vec3 verified = vec3(0.00, 0.94, 0.85); // Quantum Cyan Verified
          vec3 restored = vec3(0.48, 0.38, 1.00); // Amethyst Restored
          vColor = recoveryState < 0.5 ? lost
                 : recoveryState < 1.5 ? carved
                 : recoveryState < 2.5 ? verified : restored;
          vec4 mv = modelViewMatrix * vec4(position, 1.0);
          gl_PointSize = 2.2 * (300.0 / -mv.z);
          gl_Position = projectionMatrix * mv;
        }`,
      fragmentShader: `
        varying vec3 vColor;
        void main() {
          float d = length(gl_PointCoord - 0.5);
          if (d > 0.5) discard;
          gl_FragColor = vec4(vColor, 1.0 - d * 1.6);
        }`,
    });

    this.points = new THREE.Points(geo, mat);
    this.points.rotation.x = 0.45; // tilt disk slightly towards camera
    this.scene.add(this.points);
  }

  // Called ONLY from real engine events. Batched attribute upload.
  ignite(particleIndex, stateCode) {
    const attr = this.points.geometry.attributes.recoveryState;
    if (particleIndex >= attr.count) return;
    attr.setX(particleIndex % attr.count, stateCode);
    attr.needsUpdate = true;   // single buffer upload per batch
  }

  #bindLifecycle() {
    // Zero idle GPU burn: pause when hidden (blueprint rule)
    document.addEventListener('visibilitychange', () => {
      this.renderer.setAnimationLoop(document.hidden ? null : this.#tick);
    });

    window.addEventListener('resize', () => {
      this.camera.aspect = window.innerWidth / window.innerHeight;
      this.camera.updateProjectionMatrix();
      this.renderer.setSize(window.innerWidth, window.innerHeight);
    });

    this.renderer.setSize(window.innerWidth, window.innerHeight);
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  }

  #tick() {
    const now = performance.now();
    const dt = now - this.#lastFrame;
    this.#lastFrame = now;

    // Auto quality tier (blueprint rule): degrade gracefully, never lag
    if (dt > 16.6 && ++this.#slowFrames >= 30 && this.quality === 1) {
      this.quality = 0.5;
      this.points.geometry.setDrawRange(0, MAX_PARTICLES / 2);
      this.renderer.setPixelRatio(1);
    } else if (dt <= 16.6) {
      this.#slowFrames = 0;
    }

    this.points.rotation.y += 0.0004 * dt;  // delta-time orbit
    this.renderer.render(this.scene, this.camera);
  }
}
