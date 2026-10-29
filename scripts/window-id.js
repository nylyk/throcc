ObjC.import('CoreGraphics');
ObjC.import('Foundation');
function run(argv) {
  const list = ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0));
  const n = list.count;
  for (let i = 0; i < n; i++) {
    const w = list.objectAtIndex(i);
    const owner = ObjC.unwrap(w.objectForKey('kCGWindowOwnerName'));
    if (owner === argv[0]) return String(ObjC.unwrap(w.objectForKey('kCGWindowNumber')));
  }
  return '';
}
