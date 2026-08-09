defmodule Patala.HTTP do
  @moduledoc """
  A minimal HTTP/1.1 client over `:gen_tcp`, so this package has no dependency —
  not even `:inets`, which drags in `:ssl` and `:public_key` before it will
  answer a plain-http request.

  patala-sidecar binds `127.0.0.1` unconditionally and speaks JSON, which is
  why something this small is enough. Point a real HTTP client at
  `Patala.Sidecar.base_url/1` if you want pooling or retries; there is no TLS
  to want, because there is no network hop.
  """

  @doc """
  One request. Returns `{:ok, status, body}` — a 4xx/5xx is a **result**, not
  an error, because on this API a non-2xx is an answer with a body worth
  reading (`kind`, `error`). Only a transport failure is `{:error, reason}`.

  `body` is sent verbatim when given, so a webhook delivery can be forwarded
  byte for byte; every webhook scheme signs the bytes the processor sent, and
  re-encoding them invalidates the signature of every genuine delivery.
  """
  @spec request(
          String.t(),
          String.t(),
          String.t(),
          binary() | nil,
          [{String.t(), String.t()}],
          timeout()
        ) ::
          {:ok, non_neg_integer(), binary()} | {:error, term()}
  def request(base, method, path, body \\ nil, headers \\ [], timeout \\ 30_000)

  def request("http://" <> hostport, method, path, body, headers, timeout) do
    [host, port] = String.split(hostport, ":", parts: 2)

    case :gen_tcp.connect(
           String.to_charlist(host),
           String.to_integer(port),
           [:binary, active: false, packet: :raw],
           5_000
         ) do
      {:ok, sock} ->
        try do
          header_lines =
            for {name, value} <- headers, do: [name, ": ", value, "\r\n"]

          length_line =
            if body, do: ["Content-Length: ", Integer.to_string(byte_size(body)), "\r\n"], else: []

          request = [
            method,
            " ",
            path,
            " HTTP/1.1\r\nHost: ",
            hostport,
            "\r\nAccept: application/json\r\nConnection: close\r\n",
            header_lines,
            length_line,
            "\r\n",
            body || ""
          ]

          with :ok <- :gen_tcp.send(sock, request),
               {:ok, raw} <- read_all(sock, "", timeout) do
            parse(raw)
          end
        after
          :gen_tcp.close(sock)
        end

      {:error, reason} ->
        {:error, reason}
    end
  end

  def request(base, _method, _path, _body, _headers, _timeout),
    do: {:error, {:unsupported_scheme, base}}

  defp read_all(sock, acc, timeout) do
    case :gen_tcp.recv(sock, 0, timeout) do
      {:ok, more} -> read_all(sock, acc <> more, timeout)
      {:error, :closed} -> {:ok, acc}
      {:error, reason} -> {:error, reason}
    end
  end

  # `Connection: close` means the body runs to EOF, so the whole response is in
  # hand by the time this is called.
  defp parse(raw) do
    case :binary.split(raw, "\r\n\r\n") do
      [head, body] ->
        ["HTTP/1." <> <<_::binary-size(1)>> <> " " <> <<code::binary-size(3)>> <> _ | headers] =
          String.split(head, "\r\n")

        body =
          if Enum.any?(headers, &(String.downcase(&1) =~ "transfer-encoding: chunked")) do
            dechunk(body, "")
          else
            body
          end

        {:ok, String.to_integer(code), body}

      _ ->
        {:error, :malformed_response}
    end
  end

  defp dechunk(rest, acc) do
    case :binary.split(rest, "\r\n") do
      [size_line, tail] ->
        case Integer.parse(String.trim(size_line), 16) do
          {0, _} ->
            acc

          {size, _} ->
            <<chunk::binary-size(^size), "\r\n", tail::binary>> = tail
            dechunk(tail, acc <> chunk)

          :error ->
            acc
        end

      _ ->
        acc
    end
  end
end
