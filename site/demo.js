/* The bezel drives the production Slint UI; all sample devices stay local. */
(() => {
  const frame = document.querySelector('#slint-demo');
  const screen = document.querySelector('#demo');
  const shell = document.querySelector('.device-shell');
  const sleep = document.querySelector('#demo-sleep');
  const loading = document.querySelector('#demo-loading');
  const poster = document.querySelector('#demo-poster');
  const loadButton = document.querySelector('#demo-load');
  let ready = false, asleep = false, state = {}, interacted = false, loaded = false;
  let visible = false, tourTimer, tourIndex = 0;
  const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
  const scale = () => { frame.style.transform = `scale(${screen.clientWidth / 480})`; };
  new ResizeObserver(scale).observe(screen); scale();
  const send = name => {
    if (!ready) return;
    if (asleep) { asleep=false; sleep.hidden=true; return; }
    if (name==='power' && !state.player) { asleep=true; sleep.hidden=false; return; }
    frame.contentWindow.postMessage({couchPreview:'button',name},location.origin);
  };
  // The screen is 12.8 MB of WebAssembly. Fetch it when someone asks for it,
  // or when it scrolls into view on a wide viewport that has not asked for
  // reduced motion or data saving; a phone visitor gets the poster instead.
  const load = () => {
    if (loaded) return;
    loaded=true;poster.hidden=true;loading.hidden=false;frame.src=frame.dataset.src;
  };
  const mayAutoload = () => innerWidth>=850 && !reducedMotion.matches && !navigator.connection?.saveData;
  loadButton.addEventListener('click',load);
  // Asking for the demo is not an interaction with the device: the walkthrough
  // still runs for anyone who pressed the poster button.
  const stopTour = event => {
    if (event?.target instanceof Element && event.target.closest('#demo-poster')) return;
    interacted=true;clearTimeout(tourTimer);shell.dataset.autoplay='stopped';
    shell.querySelectorAll('.tour-pressed').forEach(b=>b.classList.remove('tour-pressed'));
  };
  // Use the same physical commands as a visitor. There is no second UI or video.
  const tour = [
    ['ok',1800], ['ok',1200], ['ok',1400], ['volume-up',1500],
    ['volume-up',1600], ['channel-up',2000], ['down',650], ['down',650],
    ['ok',2800], ['play',1800], ['play',2400], ['back-long',1600],
    ['back',1800], ['down',900], ['down',900], ['down',900], ['down',900],
    ['down',1400], ['ok',1800], ['back',1300], ['home',2200]
  ];
  const step = () => {
    if(interacted || reducedMotion.matches || !ready) return;
    if(!visible || document.hidden){tourTimer=setTimeout(step,500);return;}
    const [name,delay]=tour[tourIndex++ % tour.length];
    send(name);shell.dataset.autoplay='running';
    const button=shell.querySelector(`[data-remote="${name==='back-long'?'back':name}"]`);
    button?.classList.add('tour-pressed');
    setTimeout(()=>button?.classList.remove('tour-pressed'),name==='back-long'?650:180);
    tourTimer=setTimeout(step,delay);
  };
  const scheduleTour = () => {
    clearTimeout(tourTimer);
    if(ready && !interacted && !reducedMotion.matches) tourTimer=setTimeout(step,2200);
  };
  new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;if(visible && mayAutoload())load();},{threshold:.25}).observe(shell);
  reducedMotion.addEventListener('change',()=>{if(reducedMotion.matches){clearTimeout(tourTimer);}else{scheduleTour();}});
  shell.addEventListener('pointerdown',stopTour,{capture:true});
  shell.addEventListener('keydown',stopTour,{capture:true});
  shell.addEventListener('wheel',stopTour,{passive:true});
  document.querySelectorAll('[data-remote]').forEach(button => {
    let pressed = 0, long = false;
    button.addEventListener('pointerdown',()=>{pressed=Date.now();long=false;});
    button.addEventListener('pointerup',()=>{long=button.dataset.remote==='back' && Date.now()-pressed>=600;});
    button.addEventListener('click',()=>{stopTour();send(long?'back-long':button.dataset.remote);});
  });
  sleep.addEventListener('click',()=>{asleep=false;sleep.hidden=true;});
  addEventListener('message',event=>{
    if(event.origin!==location.origin || event.source!==frame.contentWindow) return;
    if(event.data?.couchPreview==='ready') {ready=true;loading.hidden=true;scheduleTour();}
    if(event.data?.couchPreview==='interaction') stopTour();
    if(event.data?.couchPreview==='error') loading.textContent=event.data.message;
    if(event.data?.couchPreview==='state') {
      state=event.data.state;
      document.querySelector('#demo-summary').textContent=state.player?'Tears of Steel media controls':state.room!==null?`${['Living room','Kitchen','Bedroom','Office','Dining room','Hallway'][state.room]} devices. Ceiling lights ${state.level?`on at ${state.level}%`:'off'}.`:'Home room list.';
    }
  });
})();

/* Copy buttons for the install commands. Clipboard API with a selection
   fallback; the label briefly confirms and is announced to assistive tech. */
(() => {
  const buttons = document.querySelectorAll('.copy[data-copy]');
  if (!buttons.length) return;
  const write = async (text) => {
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
        return true;
      }
    } catch (e) { /* fall through to the selection fallback */ }
    try {
      const area = document.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand('copy');
      document.body.removeChild(area);
      return ok;
    } catch (e) { return false; }
  };
  buttons.forEach((button) => {
    const original = button.textContent;
    button.addEventListener('click', async () => {
      const ok = await write(button.dataset.copy);
      button.textContent = ok ? 'Copied' : 'Press ⌘C';
      button.classList.toggle('copied', ok);
      button.setAttribute('aria-live', 'polite');
      clearTimeout(button._t);
      button._t = setTimeout(() => {
        button.textContent = original;
        button.classList.remove('copied');
      }, 2000);
    });
  });
})();
