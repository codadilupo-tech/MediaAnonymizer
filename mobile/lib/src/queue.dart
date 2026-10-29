import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import 'engine/engine.dart';
import 'settings.dart';

enum ItemStatus { pending, processing, done, failed, cancelled }

/// A photo in the queue.
class PhotoItem {
  PhotoItem(this.input, {String? name}) : name = name ?? p.basename(input);

  /// Path of the photo to process (a private copy made by the picker).
  final String input;

  /// Name shown to the user.
  final String name;

  ItemStatus status = ItemStatus.pending;
  Report? report;
  String? error;

  /// Whether the result was copied to the photo library, and why not if that failed.
  bool savedToGallery = false;
  String? galleryError;

  bool get canRetry => status == ItemStatus.failed || status == ItemStatus.cancelled;
}

/// Copies a processed photo to the device's photo library.
typedef GallerySaver = Future<void> Function(String path);

/// The list of photos and their processing state.
class QueueController extends ChangeNotifier {
  QueueController({required this.engine, required this.outputDirectory, required this.saveToGallery});

  final Engine engine;

  /// Where results are written.
  final Future<Directory> Function() outputDirectory;
  final GallerySaver saveToGallery;

  final List<PhotoItem> _items = [];
  List<PhotoItem> get items => List.unmodifiable(_items);

  CancelHandle? _cancel;
  bool get isRunning => _cancel != null;

  int get pendingCount => _items.where((i) => i.status == ItemStatus.pending).length;
  int get failedCount => _items.where((i) => i.canRetry).length;
  int get doneCount => _items.where((i) => i.status == ItemStatus.done).length;

  /// Items of the current (or last) run.
  final List<PhotoItem> _run = [];

  /// Progress of the current run: (finished, total).
  (int, int) get progress =>
      (_run.where((i) => i.status != ItemStatus.pending && i.status != ItemStatus.processing).length, _run.length);

  void add(Iterable<PhotoItem> items) {
    _items.addAll(items);
    notifyListeners();
  }

  void remove(PhotoItem item) {
    if (item.status == ItemStatus.processing) return;
    _items.remove(item);
    notifyListeners();
  }

  /// Removes finished items (their results stay in the photo library).
  void clearFinished() {
    _items.removeWhere((i) => i.status == ItemStatus.done);
    notifyListeners();
  }

  /// Processes every pending item, one at a time.
  Future<void> start(Settings settings) async {
    if (isRunning || settings.problem != null) return;
    final batch = _items.where((i) => i.status == ItemStatus.pending).toList();
    if (batch.isEmpty) return;
    final cancel = CancelHandle();
    _cancel = cancel;
    _run
      ..clear()
      ..addAll(batch);
    notifyListeners();
    try {
      final options = settings.toOptionsJson();
      final dir = await outputDirectory();
      for (final item in batch) {
        if (cancel.isCancelled) {
          item.status = ItemStatus.cancelled;
          continue;
        }
        if (!_items.contains(item)) continue;
        await _process(item, options, dir, settings.saveToGallery, cancel);
      }
    } finally {
      cancel.dispose();
      _cancel = null;
      notifyListeners();
    }
  }

  /// Requests cancellation; the photo being processed stops at the next checkpoint.
  void cancel() {
    _cancel?.cancel();
    notifyListeners();
  }

  /// Puts failed or cancelled items back in the queue.
  void retry([PhotoItem? only]) {
    for (final item in only != null ? [only] : _items) {
      if (item.canRetry) {
        item
          ..status = ItemStatus.pending
          ..error = null;
      }
    }
    notifyListeners();
  }

  Future<void> _process(PhotoItem item, String options, Directory dir, bool gallery, CancelHandle cancel) async {
    item
      ..status = ItemStatus.processing
      ..error = null
      ..report = null
      ..savedToGallery = false
      ..galleryError = null;
    notifyListeners();
    try {
      final output = await uniqueOutputPath(dir, item.name);
      item.report = await engine.process(options, item.input, output, cancel: cancel);
      item.status = ItemStatus.done;
    } on NativeException catch (e) {
      item.status = e.isCancelled ? ItemStatus.cancelled : ItemStatus.failed;
      item.error = e.isCancelled ? null : e.message;
    } catch (e) {
      item
        ..status = ItemStatus.failed
        ..error = e.toString();
    }
    notifyListeners();
    if (item.status == ItemStatus.done && gallery) await saveItem(item);
  }

  /// Copies a finished item to the photo library.
  Future<void> saveItem(PhotoItem item) async {
    final report = item.report;
    if (report == null) return;
    try {
      await saveToGallery(report.output);
      item
        ..savedToGallery = true
        ..galleryError = null;
    } catch (e) {
      item.galleryError = e.toString();
    }
    notifyListeners();
  }

  /// `<dir>/<stem>.anon<ext>`, numbered if it already exists.
  @visibleForTesting
  static Future<String> uniqueOutputPath(Directory dir, String name) async {
    await dir.create(recursive: true);
    final ext = p.extension(name);
    final stem = p.basenameWithoutExtension(name);
    var candidate = p.join(dir.path, '$stem.anon$ext');
    for (var n = 2; await File(candidate).exists(); n++) {
      candidate = p.join(dir.path, '$stem.anon-$n$ext');
    }
    return candidate;
  }
}
