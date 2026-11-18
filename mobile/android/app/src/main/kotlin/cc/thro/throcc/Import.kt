package cc.thro.throcc

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.util.Size
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts.OpenDocument
import androidx.activity.result.contract.ActivityResultContracts.RequestPermission
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.FileOpen
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.LocalLifecycleOwner
import cc.thro.throcc.core.IdentityScanner
import cc.thro.throcc.core.ScanProgress
import cc.thro.throcc.core.maxExportBytes
import java.io.IOException
import java.util.concurrent.Executors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import zxingcpp.BarcodeReader

@Composable
fun ImportChoice(model: AppModel, problem: String?) {
    val openFile = rememberExportPicker(model)
    val ways = listOf(
        Triple(Icons.Default.QrCodeScanner, "Scan QR Code", model::scanImport),
        Triple(Icons.Default.FileOpen, "Open File", openFile),
    )
    Page(model, "Import Identity", model::importBack) { padding ->
        Form(padding) {
            Text(
                "Bring your identity from another device that has throcc: open Settings, then Profile.",
                Modifier.padding(horizontal = 16.dp),
                MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Column {
                ways.forEachIndexed { index, (icon, title, onClick) ->
                    GroupRow(index, ways.size, onClick, leading = { Icon(icon, null) }) { Text(title) }
                }
            }
            problem?.let { ErrorText(it, Modifier.padding(horizontal = 16.dp)) }
        }
    }
}

@Composable
fun ImportScan(model: AppModel) {
    KeepScreenOn()
    val context = LocalContext.current
    var allowed by remember { mutableStateOf(context.allows(Manifest.permission.CAMERA)) }
    var refused by rememberSaveable { mutableStateOf(false) }
    val request = rememberLauncherForActivityResult(RequestPermission()) {
        allowed = it
        refused = !it
    }
    LaunchedEffect(Unit) { if (!allowed) request.launch(Manifest.permission.CAMERA) }
    val openFile = rememberExportPicker(model)
    val scanner = remember { IdentityScanner() }
    var progress by remember { mutableStateOf<ScanProgress?>(null) }
    Page(model, "Scan QR Code", model::importBack) { padding ->
        Form(padding) {
            if (allowed) {
                val square = Modifier.widthIn(max = 480.dp).fillMaxWidth().aspectRatio(1f)
                QrScanner(square.align(Alignment.CenterHorizontally).clip(MaterialTheme.shapes.extraLarge)) { text ->
                    if (progress?.done != true) {
                        val added = scanner.add(text)
                        progress = added
                        if (added.done) model.openImport(scanner.export())
                    }
                }
                ScanStatus(progress)
            } else if (refused) {
                Text(
                    "throcc needs the camera to scan the QR code. Allow it in the settings, or open the exported file instead.",
                    Modifier.padding(horizontal = 16.dp),
                    MaterialTheme.colorScheme.onSurfaceVariant,
                )
                FilledTonalButton(openFile, Modifier.align(Alignment.CenterHorizontally)) { Text("Open File") }
            }
        }
    }
}

@Composable
private fun ScanStatus(progress: ScanProgress?) {
    val shown by animateFloatAsState(progress?.fraction ?: 0f, label = "scan progress")
    Column(Modifier.padding(horizontal = 16.dp)) {
        Text(
            "Point the camera at the QR code on your other device.",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (progress != null) {
            LinearProgressIndicator({ shown }, Modifier.fillMaxWidth().padding(top = 16.dp, bottom = 8.dp))
            progress.size?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodySmall.copy(fontFeatureSettings = "tnum"),
                )
            }
        }
    }
}

/** The camera, which calls `onScan` with the text of every QR code it reads while the app is in front. */
@Composable
private fun QrScanner(modifier: Modifier, onScan: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scanned by rememberUpdatedState(onScan)
    val view = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }
    DisposableEffect(lifecycleOwner) {
        val main = context.mainExecutor
        val decoding = Executors.newSingleThreadExecutor()
        val reader = BarcodeReader(BarcodeReader.Options(formats = setOf(BarcodeReader.Format.QR_CODE)))
        val preview = Preview.Builder().build().apply { setSurfaceProvider(view.surfaceProvider) }
        val analysis = ImageAnalysis.Builder()
            .setResolutionSelector(
                ResolutionSelector.Builder()
                    .setResolutionStrategy(
                        ResolutionStrategy(ANALYSIS_SIZE, ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER),
                    )
                    .build(),
            )
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .build()
        analysis.setAnalyzer(decoding) { image ->
            val texts = image.use { reader.read(it) }.mapNotNull { it.text }
            if (texts.isNotEmpty()) main.execute { texts.forEach(scanned) }
        }
        val cameras = ProcessCameraProvider.getInstance(context)
        cameras.addListener({
            cameras.get().bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
        }, main)
        onDispose {
            cameras.addListener({
                cameras.get().unbind(preview, analysis)
                decoding.shutdown()
            }, main)
        }
    }
    AndroidView({ view }, modifier)
}

/** The camera frame size decoded for QR codes. */
private val ANALYSIS_SIZE = Size(1920, 1080)

@Composable
fun ImportCode(model: AppModel, entering: Importing.EnteringCode) {
    var code by rememberSaveable { mutableStateOf("") }
    val submit = { if (code.isNotBlank()) model.importIdentity(code) }
    Page(model, "Import Identity", model::importBack) { padding ->
        Form(padding) {
            Text(
                "Type the code the other device shows.",
                Modifier.padding(horizontal = 16.dp),
                MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                code,
                { code = it },
                Modifier.fillMaxWidth(),
                enabled = !entering.busy,
                textStyle = MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace),
                label = { Text("Code") },
                supportingText = entering.failure?.let { { Text(it) } },
                isError = entering.failure != null,
                keyboardOptions = KeyboardOptions(
                    KeyboardCapitalization.Characters,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Done,
                ),
                keyboardActions = KeyboardActions(onDone = { submit() }),
                singleLine = true,
            )
            MainButton("Import", code.isNotBlank(), entering.busy, submit)
        }
    }
}

/** A file picker that hands the export the user picks to `model`. */
@Composable
private fun rememberExportPicker(model: AppModel): () -> Unit {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val picker = rememberLauncherForActivityResult(OpenDocument()) { uri ->
        if (uri != null) scope.launch { model.openImport(readExport(context, uri)) }
    }
    return { picker.launch(arrayOf("*/*")) }
}

/** The file at `uri`, cut off past `maxExportBytes`, or null where it can't be read. */
private suspend fun readExport(context: Context, uri: Uri): ByteArray? = withContext(Dispatchers.IO) {
    try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            val bytes = ByteArray(maxExportBytes().toInt() + 1)
            var read = 0
            while (read < bytes.size) {
                val count = input.read(bytes, read, bytes.size - read)
                if (count < 0) break
                read += count
            }
            bytes.copyOf(read)
        }
    } catch (_: IOException) {
        null
    } catch (_: SecurityException) {
        null
    }
}

private fun Context.allows(permission: String) = checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED
