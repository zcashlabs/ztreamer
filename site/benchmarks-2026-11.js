// Adds a text filter above each table.filterable.
document.querySelectorAll('table.filterable').forEach(t=>{
  const input=document.createElement('input');input.type='search';input.className='table-filter';input.placeholder='Filter rows, e.g. GetBlockRange';
  input.setAttribute('aria-label','Filter rows');t.parentElement.before(input);
  const rows=[...t.tBodies[0].rows];
  input.addEventListener('input',()=>{const q=input.value.toLowerCase();rows.forEach(r=>r.hidden=!r.textContent.toLowerCase().includes(q))});
});
// Server toggles: hide-<srv> on body hides every [data-srv] part of that server, and each plot
// switches to the SVG drawn for the servers that are on (benchmarks/<date>/v/<key>/).
(()=>{
  const boxes=[...document.querySelectorAll('input[data-srv-toggle]')];
  if(!boxes.length)return;
  const bar=document.querySelector('.server-toggles'),head=document.querySelector('header');
  const top=()=>bar.style.setProperty('--toggles-top',(head?head.offsetHeight:0)+'px');
  top();addEventListener('resize',top);
  const apply=()=>{
    const on=boxes.filter(b=>b.checked).map(b=>b.dataset.srvToggle);
    boxes.forEach(b=>document.body.classList.toggle('hide-'+b.dataset.srvToggle,!b.checked));
    const key=on.join('-')||'none';
    document.querySelectorAll('img[data-plot]').forEach(i=>{const src=`${i.dataset.base}v/${key}/${i.dataset.plot}.svg?v=${i.dataset.ver}`;if(i.getAttribute('src')!==src)i.src=src});
    const q=new URLSearchParams(location.search);q.set('vs',key);history.replaceState(null,'',`${location.pathname}?${q}${location.hash}`);
  };
  const vs=new URLSearchParams(location.search).get('vs');
  if(vs)boxes.forEach(b=>b.checked=vs.split('-').includes(b.dataset.srvToggle));
  boxes.forEach(b=>b.addEventListener('change',apply));
  apply();
})();
