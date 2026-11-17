package cc.thro.throcc

import android.content.Context
import android.content.Intent
import android.view.WindowManager
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts.StartActivityForResult
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.Icon
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import cc.thro.throcc.core.QrFrame
import cc.thro.throcc.core.qrFrameMillis
import java.io.File
import kotlin.math.floor
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private enum class ExportWay { Qr, File }

private const val EXPORT_TITLE = "Use on Another Device"

/** The ways to take this identity to another device, at the bottom of the profile page. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun ExportSection(model: AppModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var preparing by remember { mutableStateOf<ExportWay?>(null) }
    val share = rememberLauncherForActivityResult(StartActivityForResult()) { model.showExportCode() }
    val export = { way: ExportWay ->
        if (preparing == null) {
            preparing = way
            scope.launch {
                val sealed = model.startExport()
                when {
                    sealed == null -> {}
                    way == ExportWay.Qr -> model.open(Screen.ExportQr)
                    else -> {
                        val file = exportFile(context)
                        withContext(Dispatchers.IO) { file.writeBytes(sealed.file()) }
                        share.launch(Intent.createChooser(sendIntent(context, file), null))
                    }
                }
                preparing = null
            }
        }
    }
    val ways = listOf(
        Triple(ExportWay.Qr, Icons.Default.QrCode, "Show QR Code"),
        Triple(ExportWay.File, Icons.Default.Share, "Share File"),
    )
    Column {
        GroupTitle(EXPORT_TITLE)
        Text(
            "Use your identity on another device. It gets a copy of the people you verified or blocked, then keeps its own lists. Anyone who gets the export and its code can sign in as you.",
            Modifier.padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
            MaterialTheme.colorScheme.onSurfaceVariant,
        )
        ways.forEachIndexed { index, (way, icon, title) ->
            GroupRow(
                index,
                ways.size,
                { export(way) },
                leading = { Icon(icon, null) },
                trailing = { if (preparing == way) LoadingIndicator(Modifier.size(24.dp)) },
            ) { Text(title) }
        }
    }
}

/** The export's QR codes, one after the other, until the user asks for its code. */
@Composable
fun ExportQr(model: AppModel) {
    val export = remember { model.export } ?: return
    var frame by remember { mutableStateOf<QrFrame?>(null) }
    LaunchedEffect(export) {
        val millis = qrFrameMillis().toLong()
        var seq = 0u
        while (true) {
            frame = withContext(Dispatchers.Default) { export.frame(seq++) }
            delay(millis)
        }
    }
    KeepScreenOn()
    Page(model, EXPORT_TITLE) { padding ->
        Form(padding) {
            val square = Modifier.widthIn(max = 480.dp).fillMaxWidth().aspectRatio(1f).align(Alignment.CenterHorizontally)
            frame?.let { QrCode(it, square) }
            Text(
                "Scan this with throcc on the new device.",
                Modifier.fillMaxWidth(),
                MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            MainButton("Show Code", onClick = model::showExportCode)
        }
    }
}

/** The code that opens the export, which the QR codes never show beside. */
@Composable
fun ExportCode(model: AppModel) {
    val export = remember { model.export } ?: return
    val code = remember(export) { export.code() }
    val context = LocalContext.current
    DisposableEffect(Unit) {
        onDispose { exportFile(context).delete() }
    }
    Page(model, EXPORT_TITLE) { padding ->
        Form(padding) {
            Panel {
                Text(
                    code,
                    Modifier.fillMaxWidth(),
                    fontFamily = FontFamily.Monospace,
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.headlineLarge,
                )
                Text(
                    "Type this code on the new device.",
                    Modifier.fillMaxWidth(),
                    MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            MainButton("Done", onClick = model::finishExport)
        }
    }
}

/** Dark modules on white, whatever the theme, as scanners need. */
@Composable
private fun QrCode(frame: QrFrame, modifier: Modifier) {
    val width = frame.width.toInt()
    Canvas(modifier.clip(MaterialTheme.shapes.large).background(Color.White)) {
        val module = floor(size.minDimension / width).coerceAtLeast(1f)
        val origin = Offset(floor((size.width - module * width) / 2), floor((size.height - module * width) / 2))
        val square = Size(module, module)
        frame.dark.forEachIndexed { index, dark ->
            if (dark) drawRect(Color.Black, origin + Offset(index % width * module, index / width * module), square)
        }
    }
}

/** Where the export waits while another app takes it. The file provider shares only this folder. */
private fun exportFile(context: Context) =
    File(context.cacheDir, "export").apply { mkdirs() }.resolve("throcc-identity.throcc")

/** Removes an export file a share left behind when the process last ended. */
fun removeLeftoverExport(context: Context) {
    exportFile(context).delete()
}

private fun sendIntent(context: Context, file: File) = Intent(Intent.ACTION_SEND)
    .setType("application/octet-stream")
    .putExtra(Intent.EXTRA_STREAM, FileProvider.getUriForFile(context, "${context.packageName}.files", file))
    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)

/** Keeps the window out of screenshots, recordings, casts and the recents thumbnail while shown. */
@Composable
fun SecureScreen() {
    val window = LocalActivity.current?.window ?: return
    DisposableEffect(window) {
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        onDispose { window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE) }
    }
}
