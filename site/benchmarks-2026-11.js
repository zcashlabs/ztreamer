// Adds a text filter above each table.filterable.
document.querySelectorAll('table.filterable').forEach(t=>{
  const input=document.createElement('input');input.type='search';input.className='table-filter';input.placeholder='Filter rows, e.g. GetBlockRange';
  input.setAttribute('aria-label','Filter rows');t.parentElement.before(input);
  const rows=[...t.tBodies[0].rows];
  input.addEventListener('input',()=>{const q=input.value.toLowerCase();rows.forEach(r=>r.hidden=!r.textContent.toLowerCase().includes(q))});
});
