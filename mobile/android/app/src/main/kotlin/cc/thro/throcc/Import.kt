package cc.thro.throcc

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts.OpenDocument
import androidx.activity.result.contract.ActivityResultContracts.RequestPermission
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
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import cc.thro.throcc.core.IdentityScanner
import cc.thro.throcc.core.ScanProgress
import com.google.zxing.BarcodeFormat
import com.google.zxing.ResultPoint
import com.journeyapps.barcodescanner.BarcodeCallback
import com.journeyapps.barcodescanner.BarcodeResult
import com.journeyapps.barcodescanner.BarcodeView
import com.journeyapps.barcodescanner.DefaultDecoderFactory
import java.io.IOException
import kotlin.math.ceil
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** The most bytes an export file can hold, far more than any does. */
const val MAX_EXPORT_BYTES = 4 shl 20

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
                "Bring your identity from a device that has throcc. On that device, open Settings, then Profile.",
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
                    "throcc needs the camera to scan the QR code. Allow it in the system settings, " +
                        "or open the exported file instead.",
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
    Column(Modifier.padding(horizontal = 16.dp)) {
        Text(
            "Point the camera at the QR code on the other device.",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (progress != null) {
            LinearProgressIndicator({ progress.fraction }, Modifier.fillMaxWidth().padding(top = 16.dp, bottom = 8.dp))
            progress.remainingSeconds?.let {
                Text(
                    "About ${ceil(it).toInt()} s left",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

/** The camera, which calls `onScan` with the text of every QR code it reads while the app is in front. */
@Composable
private fun QrScanner(modifier: Modifier, onScan: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val scanned by rememberUpdatedState(onScan)
    val camera = remember {
        BarcodeView(context).apply {
            setUseTextureView(true)
            decoderFactory = DefaultDecoderFactory(listOf(BarcodeFormat.QR_CODE))
            decodeContinuous(object : BarcodeCallback {
                override fun barcodeResult(result: BarcodeResult) = scanned(result.text)

                override fun possibleResultPoints(resultPoints: List<ResultPoint>) {}
            })
        }
    }
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_RESUME -> camera.resume()
                Lifecycle.Event.ON_PAUSE -> camera.pause()
                else -> {}
            }
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            camera.pause()
        }
    }
    AndroidView({ camera }, modifier)
}

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

/** The file at `uri`, cut off past `MAX_EXPORT_BYTES`, or null where it can't be read. */
private suspend fun readExport(context: Context, uri: Uri): ByteArray? = withContext(Dispatchers.IO) {
    try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            val bytes = ByteArray(MAX_EXPORT_BYTES + 1)
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
