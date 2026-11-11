package cc.thro.throcc

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.identityKey

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val identity = runCatching { identityKey() }
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    IdentityScreen(identity)
                }
            }
        }
    }
}

@Composable
private fun IdentityScreen(identity: Result<ByteArray>) {
    Column(
        modifier = Modifier.fillMaxSize().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("Identity key", style = MaterialTheme.typography.titleMedium)
        identity.fold(
            onSuccess = { key ->
                SelectionContainer {
                    Text(
                        key.joinToString("") { "%02x".format(it) },
                        fontFamily = FontFamily.Monospace,
                        textAlign = TextAlign.Center,
                    )
                }
            },
            onFailure = { error ->
                Text(
                    error.message ?: error.toString(),
                    color = MaterialTheme.colorScheme.error,
                    textAlign = TextAlign.Center,
                )
            },
        )
    }
}
