# 11.ソースコードの書き換え

> このspecだけnennneko5787が書いています。

## どういうことか

コミット `1e837a04d79adc9048a6f73a989655c18b3dc877` より後のコミットはClaude Code以外のAI(`bfe670103cb8b4a61a54c5f99857f44f8c2289b9`までは`stealth/ox-alpha`(実際は`z-ai/glm-5.3-flash`)、それ以降はopencode zenのbig pickle)によって書かれているため、コーディングやUIのデザイン、API実装がルールに沿っていない可能性がある。

## どうすべきか

**Claude Code** が `1e837a04d79adc9048a6f73a989655c18b3dc877` より後のコミットを精査し、悪いところがあれば修正する必要がある。  
  
  
ちなみに、コミット `e34215d5665f4fb21f5021651e6a519d6c8c39be`、コミット `d0fde9eec7caed771d1f345a07d02ccf9580886b`、コミット `6c7fd29a7ed765d4cc852ac94065bfe94d27601b`は人間による編集もしくはClaude Codeによる編集である。