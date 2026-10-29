// End-to-end tests of the Dart bindings against the real native library.
//
// They need a host build of crates/mobile; run them with
//   cargo build -p mediaanon-mobile
//   MEDIAANON_MOBILE_LIB=../target/debug/libmediaanon_mobile.so flutter test
// (`.dylib` on macOS). Without the variable they are skipped.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:mediaanonymizer/src/engine/engine.dart';
import 'package:mediaanonymizer/src/engine/native.dart' show libraryOverrideVariable;
import 'package:mediaanonymizer/src/queue.dart';
import 'package:mediaanonymizer/src/settings.dart';
import 'package:path/path.dart' as p;

final String? _library = Platform.environment[libraryOverrideVariable];
final _skip = _library == null || _library!.isEmpty ? 'set $libraryOverrideVariable to run' : null;

/// A portrait with GPS coordinates in its EXIF data.
final _fixture = p.join('..', 'crates', 'core', 'tests', 'fixtures', 'astronaut_gps.jpg');

void main() {
  const engine = Engine();
  late Directory tmp;

  setUp(() async => tmp = await Directory.systemTemp.createTemp('mediaanon_test'));
  tearDown(() async => tmp.delete(recursive: true));

  test('reports its version', () => expect(engine.version(), matches(RegExp(r'^\d+\.\d+\.\d+'))), skip: _skip);

  test('inspect flags GPS as sensitive', () async {
    final inspection = await engine.inspect(_fixture);
    expect(inspection.format, 'JPEG');
    expect(inspection.findings.where((f) => f.sensitive && f.detail.contains('GPS')), isNotEmpty);
  }, skip: _skip);

  test('process removes metadata and redacts the face', () async {
    final out = p.join(tmp.path, 'out.jpg');
    final report = await engine.process(const Settings().toOptionsJson(), _fixture, out);
    expect(report.faces, greaterThanOrEqualTo(1));
    expect(report.removed, isNotEmpty);
    expect(report.output, out);
    expect(report.outputBytes, File(out).lengthSync());
    final after = await engine.inspect(out);
    expect(after.sensitiveCount, 0);
  }, skip: _skip);

  test('preview renders a JPEG', () async {
    final out = p.join(tmp.path, 'preview.jpg');
    final preview = await engine.preview(const Settings(timestamp: true).toOptionsJson(), _fixture, out, maxSide: 320);
    expect(preview.faces, greaterThanOrEqualTo(1));
    expect([preview.width, preview.height].reduce((a, b) => a > b ? a : b), lessThanOrEqualTo(320));
    expect(File(out).readAsBytesSync().sublist(0, 2), [0xFF, 0xD8]);
  }, skip: _skip);

  test('cancellation surfaces as a cancelled error', () async {
    final cancel = CancelHandle()..cancel();
    await expectLater(
      engine.process(const Settings().toOptionsJson(), _fixture, p.join(tmp.path, 'x.jpg'), cancel: cancel),
      throwsA(isA<NativeException>().having((e) => e.isCancelled, 'isCancelled', isTrue)),
    );
    cancel.dispose();
  }, skip: _skip);

  test('unsupported files fail with a readable message', () async {
    final text = File(p.join(tmp.path, 'notes.txt'))..writeAsStringSync('hello');
    await expectLater(
      engine.inspect(text.path),
      throwsA(isA<NativeException>().having((e) => e.kind, 'kind', 'unsupported')),
    );
  }, skip: _skip);

  test('queue processes photos, saves them and reports failures', () async {
    final saved = <String>[];
    final queue = QueueController(
      engine: engine,
      outputDirectory: () async => Directory(p.join(tmp.path, 'out')),
      saveToGallery: (path) async => saved.add(path),
    );
    final broken = File(p.join(tmp.path, 'broken.jpg'))..writeAsBytesSync([0xFF, 0xD8, 0xFF, 0xE0, 0, 2]);
    queue.add([PhotoItem(_fixture, name: 'portrait.jpg'), PhotoItem(broken.path)]);
    await queue.start(const Settings());

    final [ok, failed] = queue.items;
    expect(ok.status, ItemStatus.done);
    expect(p.basename(ok.report!.output), 'portrait.anon.jpg');
    expect(ok.savedToGallery, isTrue);
    expect(saved, [ok.report!.output]);
    expect(failed.status, ItemStatus.failed);
    expect(failed.error, isNotEmpty);
    expect(queue.progress, (2, 2));
    expect(queue.isRunning, isFalse);

    // A second photo with the same name does not overwrite the first result.
    queue.add([PhotoItem(_fixture, name: 'portrait.jpg')]);
    await queue.start(const Settings(saveToGallery: false));
    expect(p.basename(queue.items.last.report!.output), 'portrait.anon-2.jpg');
    expect(saved, hasLength(1));
  }, skip: _skip);
}
