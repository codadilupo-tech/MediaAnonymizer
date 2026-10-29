/// Asynchronous API over the native engine. Every call runs on a background
/// isolate so the UI never blocks.
library;

import 'dart:isolate';

import 'native.dart';

export 'native.dart' show NativeException;

/// One metadata block found in a photo.
class Finding {
  const Finding({required this.block, required this.detail, required this.sensitive});

  factory Finding.fromJson(Map<String, dynamic> json) =>
      Finding(block: json['block'] as String, detail: json['detail'] as String, sensitive: json['sensitive'] as bool);

  /// Container-level block, e.g. `APP1 EXIF`.
  final String block;
  final String detail;

  /// Whether "Remove sensitive" removes it.
  final bool sensitive;
}

/// Result of [Engine.inspect].
class Inspection {
  const Inspection({required this.format, required this.findings});

  factory Inspection.fromJson(Map<String, dynamic> json) => Inspection(
    format: json['format'] as String,
    findings: [for (final f in json['findings'] as List) Finding.fromJson(f as Map<String, dynamic>)],
  );

  /// `JPEG`, `PNG`, `WebP`...
  final String format;
  final List<Finding> findings;

  int get sensitiveCount => findings.where((f) => f.sensitive).length;
}

/// Summary of one processed photo (`mediaanon_core::Report`).
class Report {
  const Report({
    required this.output,
    required this.faces,
    required this.timestamp,
    required this.removed,
    required this.warnings,
    required this.notes,
    required this.inputBytes,
    required this.outputBytes,
    required this.elapsed,
  });

  factory Report.fromJson(Map<String, dynamic> json) => Report(
    output: json['output'] as String,
    faces: json['faces'] as int,
    timestamp: json['timestamp'] as String?,
    removed: List<String>.from(json['removed'] as List),
    warnings: List<String>.from(json['warnings'] as List),
    notes: List<String>.from(json['notes'] as List),
    inputBytes: json['input_bytes'] as int,
    outputBytes: json['output_bytes'] as int,
    elapsed: Duration(microseconds: ((json['elapsed'] as num) * 1e6).round()),
  );

  final String output;
  final int faces;
  final String? timestamp;
  final List<String> removed;
  final List<String> warnings;
  final List<String> notes;
  final int inputBytes;
  final int outputBytes;
  final Duration elapsed;
}

/// Result of [Engine.preview].
class Preview {
  const Preview({required this.path, required this.faces, required this.width, required this.height});

  final String path;
  final int faces;
  final int width;
  final int height;
}

/// Cancels the operations it is passed to. Call [dispose] when done.
class CancelHandle {
  CancelHandle() : _id = NativeLibrary.instance.cancelNew();

  final int _id;
  bool _cancelled = false;

  bool get isCancelled => _cancelled;

  void cancel() {
    _cancelled = true;
    NativeLibrary.instance.cancel(_id);
  }

  void dispose() => NativeLibrary.instance.cancelFree(_id);
}

/// The anonymization engine (`mediaanon-core`).
class Engine {
  const Engine();

  /// Version of the native library; throws if it cannot be loaded.
  String version() => NativeLibrary.instance.version();

  /// Lists the metadata found in a photo.
  Future<Inspection> inspect(String path) async {
    final json = await Isolate.run(() => NativeLibrary.instance.inspect(path));
    return Inspection.fromJson(json! as Map<String, dynamic>);
  }

  /// Processes [input] into [output] with the JSON [options]
  /// (`mediaanon_core::Options`).
  Future<Report> process(String options, String input, String output, {CancelHandle? cancel}) async {
    final id = cancel?._id ?? 0;
    final json = await Isolate.run(() => NativeLibrary.instance.process(options, input, output, id));
    return Report.fromJson(json! as Map<String, dynamic>);
  }

  /// Renders a redacted preview of [input] as a JPEG at [output].
  Future<Preview> preview(String options, String input, String output, {int maxSide = 1600}) async {
    final json = await Isolate.run(
      () => NativeLibrary.instance.preview(options, input, output, maxSide),
    ) as Map<String, dynamic>;
    return Preview(
      path: output,
      faces: json['faces'] as int,
      width: json['width'] as int,
      height: json['height'] as int,
    );
  }
}
