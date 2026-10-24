"""Bounded OOXML inspection and edit-copy. Standard library only; no Office macros run."""
import hashlib
import io
import json
import math
import os
from pathlib import Path
import posixpath
import re
import sys
import tempfile
import zipfile
from xml.dom import minidom

W = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main'
S = 'http://schemas.openxmlformats.org/spreadsheetml/2006/main'
R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships'
MAX_FILE = 32 * 1024 * 1024


def elements(node, namespace, name):
    return list(node.getElementsByTagNameNS(namespace, name))


def text(node):
    return ''.join(child.data if child.nodeType in (child.TEXT_NODE, child.CDATA_SECTION_NODE) else text(child)
                   for child in node.childNodes)


def set_text(node, value):
    if any((ord(c)<32 and c not in '\n\r\t') or 0xD800<=ord(c)<=0xDFFF or ord(c) in (0xFFFE,0xFFFF) for c in value):
        raise ValueError('replacement contains characters forbidden in XML')
    for child in list(node.childNodes):
        node.removeChild(child)
    node.appendChild(node.ownerDocument.createTextNode(value))
    node.setAttribute('xml:space', 'preserve')


def xml(archive, name):
    info = archive.getinfo(name)
    if info.file_size > 8 * 1024 * 1024:
        raise ValueError('XML part exceeds 8 MiB limit')
    data = archive.read(name)
    if b'\x00' in data or b'<!DOCTYPE' in data.upper() or b'<!ENTITY' in data.upper():
        raise ValueError('UTF-16 XML and DTD/entity declarations are not supported')
    return minidom.parseString(data)


def sheet_map(archive):
    workbook = xml(archive, 'xl/workbook.xml')
    rels = xml(archive, 'xl/_rels/workbook.xml.rels')
    targets = {}
    for rel in rels.documentElement.childNodes:
        if rel.nodeType != rel.ELEMENT_NODE or rel.getAttribute('TargetMode') == 'External':
            continue
        target = rel.getAttribute('Target')
        target = posixpath.normpath(target.lstrip('/') if target.startswith('/') else 'xl/' + target)
        if not target.startswith('xl/') or '..' in target.split('/'):
            raise ValueError('unsafe worksheet relationship')
        targets[rel.getAttribute('Id')] = target
    sheets = {}
    for sheet in elements(workbook, S, 'sheet'):
        target = targets.get(sheet.getAttributeNS(R, 'id'))
        if target:
            sheets[sheet.getAttribute('name')] = target
    return workbook, sheets


def address(value):
    match = re.fullmatch(r'([A-Z]{1,3})([1-9][0-9]{0,6})', str(value))
    if not match:
        raise ValueError('cell must be an A1 reference without $ signs')
    column = 0
    for letter in match[1]:
        column = column * 26 + ord(letter) - 64
    row = int(match[2])
    if column > 16384 or row > 1048576:
        raise ValueError('cell exceeds Excel row/column bounds')
    return row, column


def child(parent, name):
    prefix = parent.prefix or parent.ownerDocument.documentElement.prefix
    result = parent.ownerDocument.createElementNS(S, (prefix + ':' if prefix else '') + name)
    following = ('oleSize','customWorkbookViews','pivotCaches','smartTagPr','smartTagTypes','webPublishing','fileRecoveryPr','webPublishObjects','extLst') if name=='calcPr' else ('extLst',)
    before = next((node for node in parent.childNodes if node.nodeType==node.ELEMENT_NODE and node.namespaceURI==S and node.localName in following),None)
    parent.insertBefore(result,before) if before else parent.appendChild(result)
    return result


def docx(archive, args, editing):
    dom = xml(archive, 'word/document.xml')
    paragraphs = elements(dom, W, 'p')
    if not editing:
        offset = max(1, int(args.get('offset', 1)))
        limit = min(100, max(1, int(args.get('limit', 50))))
        rows = []
        for index, paragraph in enumerate(paragraphs[offset-1:offset-1+limit], offset):
            value = ''.join(text(n) for n in elements(paragraph, W, 't'))
            rows.append({'paragraph': index, 'text': value[:1000], 'truncated': len(value)>1000})
        return {'paragraphs': rows, 'total_paragraphs': len(paragraphs), 'truncated': offset-1+limit<len(paragraphs)}, {}
    replacements = args.get('replacements')
    if not isinstance(replacements, list) or not 1 <= len(replacements) <= 50:
        raise ValueError('DOCX requires 1–50 replacements')
    # Resolve all replacements against the original paragraphs before modifying.
    changes = {}
    for replacement in replacements:
        old, new = replacement.get('old_text'), replacement.get('new_text')
        if not isinstance(old, str) or not old or not isinstance(new, str) or any(c in old+new for c in '\n\r\t'):
            raise ValueError('replacements require nonempty old_text and string new_text without tabs/newlines')
        found = []
        for index, paragraph in enumerate(paragraphs):
            value = ''.join(text(n) for n in elements(paragraph, W, 't'))
            start = 0
            while (start := value.find(old, start)) >= 0:
                found.append((index, start, start+len(old), new))
                start += 1
        if len(found) != 1:
            raise ValueError('old_text must occur exactly once within a paragraph; found '+str(len(found)))
        index, start, end, new = found[0]
        if any(elements(paragraphs[index],W,name) for name in ('tab','br','cr','fldChar','instrText','ins','del')):
            raise ValueError('paragraph contains breaks, fields or tracked changes; editing it is not supported')
        changes.setdefault(index, []).append((start,end,new))
    for index, edits in changes.items():
        edits.sort()
        if any(a[1] > b[0] for a,b in zip(edits,edits[1:])):
            raise ValueError('DOCX replacements overlap')
        nodes = elements(paragraphs[index], W, 't')
        for start,end,new in reversed(edits):
            position = 0
            inserted = False
            for node in nodes:
                value = text(node)
                last = position + len(value)
                if position < end and last > start:
                    prefix = value[:max(0,start-position)]
                    suffix = value[max(0,end-position):]
                    set_text(node, prefix + (new if not inserted else '') + suffix)
                    inserted = True
                position = last
    return {'replacements': len(replacements)}, {'word/document.xml': dom.toxml(encoding='utf-8')}


def xlsx(archive, args, editing):
    workbook, sheets = sheet_map(archive)
    if not editing and not args.get('sheet'):
        return {'sheets': list(sheets), 'note': 'Choose a sheet to read cells. Formula values are saved caches, not recalculated.'}, {}
    sheet = args.get('sheet')
    if sheet not in sheets:
        raise ValueError('unknown sheet name')
    part = sheets[sheet]
    dom = xml(archive, part)
    cells = {cell.getAttribute('r'):cell for cell in elements(dom,S,'c')}
    if not editing:
        shared = []
        if 'xl/sharedStrings.xml' in archive.namelist():
            shared = [''.join(text(t) for t in elements(si,S,'t')) for si in elements(xml(archive,'xl/sharedStrings.xml'),S,'si')]
        start = max(1,int(args.get('offset',1)))
        limit = min(100,max(1,int(args.get('limit',50))))
        rows = []
        selected = sorted(cells.items(),key=lambda item:address(item[0]))
        for reference,cell in selected[start-1:start-1+limit]:
            values = elements(cell,S,'v')
            value = text(values[0]) if values else ''
            if cell.getAttribute('t') == 's':
                value = shared[int(value)]
            elif cell.getAttribute('t') == 'inlineStr':
                value = ''.join(text(t) for t in elements(cell,S,'t'))
            formulas = elements(cell,S,'f')
            rows.append({'cell':reference,'value':value[:1000],'type':cell.getAttribute('t') or 'number',
                         'formula':text(formulas[0])[:1000] if formulas else None,'truncated':len(value)>1000})
        return {'sheet':sheet,'cells':rows,'total_cells':len(cells),'truncated':start-1+limit<len(cells),
                'note':'Only stored cells are returned. Dates may be Excel serial numbers; formula results are saved caches.'}, {}
    edits = args.get('cells')
    if not isinstance(edits,list) or not 1 <= len(edits) <= 100:
        raise ValueError('XLSX requires 1–100 cell edits in one sheet')
    seen = set()
    for edit in edits:
        ref = edit.get('cell')
        row_number,col = address(ref)
        if ref in seen: raise ValueError('duplicate cell edit')
        seen.add(ref)
        for merged in elements(dom,S,'mergeCell'):
            bounds = merged.getAttribute('ref').split(':')
            if len(bounds)==2:
                (r1,c1),(r2,c2) = map(address,bounds)
                if r1<=row_number<=r2 and c1<=col<=c2 and ref!=bounds[0]:
                    raise ValueError('edit the top-left cell of a merged range')
        cell = cells.get(ref)
        if cell is None:
            raise ValueError('cell '+ref+' is not stored in this sheet; adding cells is not supported yet')
        if 'formula' in edit or cell.getAttribute('t') == 'e' or elements(cell,S,'f'):
            raise ValueError('formula/error cells are read-only; only literal cell values may be edited')
        value = edit.get('value')
        if 'value' not in edit or not (value is None or isinstance(value,(str,int,float,bool))):
            raise ValueError('value must be text, number, boolean or null')
        if isinstance(value,float) and not math.isfinite(value): raise ValueError('number must be finite')
        for node in list(cell.childNodes):
            if node.nodeType==node.ELEMENT_NODE and node.namespaceURI==S and node.localName in ('v','is','f'):
                cell.removeChild(node)
        cell.removeAttribute('t') if cell.hasAttribute('t') else None
        if isinstance(value,str):
            cell.setAttribute('t','inlineStr')
            inline = child(cell,'is'); node = child(inline,'t'); set_text(node,value)
        elif isinstance(value,bool):
            cell.setAttribute('t','b'); child(cell,'v').appendChild(dom.createTextNode('1' if value else '0'))
        elif value is not None:
            child(cell,'v').appendChild(dom.createTextNode(str(value)))
    calc = elements(workbook,S,'calcPr')
    calc = calc[0] if calc else child(workbook.documentElement,'calcPr')
    calc.setAttribute('fullCalcOnLoad','1'); calc.setAttribute('forceFullCalc','1')
    updates = {part:dom.toxml(encoding='utf-8'),'xl/workbook.xml':workbook.toxml(encoding='utf-8')}
    # Cached calculation chains can refer to cells whose values changed. Leave
    # formulas untouched; ask the application to recalculate on opening.
    return {'sheet':sheet,'cells_updated':len(edits),'note':'Open the copy in Excel/LibreOffice to recalculate formulas.'}, updates


def run(args):
    root = Path(args.pop('_root')).resolve(strict=True)
    source = Path(args['path']).resolve(strict=True)
    if not source.is_relative_to(root) or not source.is_file(): raise ValueError('source is outside workspace or not a file')
    if source.suffix.lower() not in ('.docx','.xlsx'): raise ValueError('only .docx and .xlsx are supported')
    with source.open('rb') as file:
        data = file.read(MAX_FILE+1)
    if len(data)>MAX_FILE: raise ValueError('document exceeds 32 MiB')
    digest = hashlib.sha256(data).hexdigest()
    editing = args.pop('_edit',False)
    if editing and args.get('expected_sha256') != digest: raise ValueError('document changed or fingerprint missing; read it again before editing')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        infos = archive.infolist()
        if len(infos)>5000 or sum(info.file_size for info in infos)>64*1024*1024: raise ValueError('document expands beyond limits')
        if len({info.filename for info in infos})!=len(infos): raise ValueError('duplicate ZIP parts are not supported')
        if any(info.flag_bits&1 or 'vbaproject' in info.filename.lower() or info.filename.lower().startswith('_xmlsignatures/') for info in infos):
            raise ValueError('encrypted, macro-enabled or signed documents are not supported')
        result,updates = (docx if source.suffix.lower()=='.docx' else xlsx)(archive,args,editing)
        result.update({'path':str(source),'format':source.suffix.lower()[1:],'sha256':digest,'state':'saved-on-disk'})
        if editing:
            output = Path(args['output_path']).resolve()
            if not output.is_relative_to(root) or output==source or output.suffix.lower()!=source.suffix.lower():
                raise ValueError('output must be a different path of the same format inside the workspace')
            if not output.parent.is_dir(): raise ValueError('output parent directory must already exist')
            if output.exists(): raise ValueError('output already exists; choose a new name')
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(dir=output.parent,prefix='.drops-office-',delete=False) as file:
                    temporary = file.name
                    with zipfile.ZipFile(file,'w') as target:
                        for info in infos:
                            target.writestr(info,updates.get(info.filename,archive.read(info.filename)))
                os.link(temporary,output)  # atomic publication, never overwrite an existing path
            finally:
                if temporary is not None: os.unlink(temporary)
            result['output_path'] = str(output)
            result['output_sha256'] = hashlib.sha256(output.read_bytes()).hexdigest()
        return result


if __name__=='__main__':
    try:
        args = json.loads(sys.stdin.read(1024*1024))
        result = run(args)
        for key in ('paragraphs','cells','sheets'):
            while len(json.dumps(result,ensure_ascii=False).encode())>45000 and result.get(key):
                result[key].pop()
                result['truncated'] = True
        print(json.dumps({'result':result},ensure_ascii=False))
    except Exception as error:
        print(json.dumps({'error':str(error)}))
        sys.exit(1)
