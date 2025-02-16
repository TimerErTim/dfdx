use cudarc::cublas::{CudaBlas, Gemm};
use cudarc::driver::{AsKernelParam, LaunchAsync, LaunchConfig, ValidAsZeroBits};

use crate::tensor_ops::matmul::cuda_kernel::sgemm_batch;
use crate::{shapes::*, tensor::cuda::Cuda};

use std::sync::Arc;

unsafe impl AsKernelParam for super::Conv2DOp {}

const PTX_SRC: &str = include_str!(concat!(env!("OUT_DIR"), "/conv2d.ptx"));

trait HasCudaKernel<E> {
    const MOD: &'static str;
    const FNS: &'static [&'static str];
}

impl HasCudaKernel<f32> for Cuda {
    const MOD: &'static str = "conv2d_f32";
    const FNS: &'static [&'static str] = &[
        "unfold_input_into_patches_f32",
        "unfold_output_into_patches_f32",
        "transpose_and_broadcast_filters_f32",
        "sum_transposed_filters_f32",
    ];
}

impl HasCudaKernel<f64> for Cuda {
    const MOD: &'static str = "conv2d_f64";
    const FNS: &'static [&'static str] = &[
        "unfold_input_into_patches_f64",
        "unfold_output_into_patches_f64",
        "transpose_and_broadcast_filters_f64",
        "sum_transposed_filters_f64",
    ];
}

fn make_4d<S: Shape>(strides: S::Concrete) -> [usize; 4] {
    match S::NUM_DIMS {
        3 => [0, strides[0], strides[1], strides[2]],
        4 => [strides[0], strides[1], strides[2], strides[3]],
        _ => unreachable!("Only implemented for 3d & 4d arrays"),
    }
}

impl<E: Dtype + ValidAsZeroBits> super::Conv2DKernel<E> for Cuda
where
    Self: HasCudaKernel<E>,
    CudaBlas: Gemm<E>,
{
    fn forward<L: Shape, R: Shape, O: Shape>(
        &self,
        op: super::Conv2DOp,
        lhs: &Self::Storage<L, E>,
        rhs: &Self::Storage<R, E>,
        out: &mut Self::Storage<O, E>,
    ) -> Result<(), Self::Err> {
        if !self.dev.has_func(Self::MOD, Self::FNS[0]) {
            self.dev.load_ptx(PTX_SRC.into(), Self::MOD, Self::FNS)?;
        }

        let patches_numel = op.batch * op.chan_in * op.kernel * op.kernel * op.h_out * op.w_out;
        let mut patches = self.dev.alloc_zeros_async::<E>(patches_numel)?;
        let img_strides = self.dev.take_async(make_4d::<L>(lhs.strides).into())?;
        let unfold_fn = self.dev.get_func(Self::MOD, Self::FNS[0]).unwrap();
        let cfg = LaunchConfig::for_num_elems(patches.len() as u32);
        let params = (op, lhs.data.as_ref(), &img_strides, &mut patches);
        unsafe { unfold_fn.launch_async(cfg, params) }?;

        // (O, C * K * K) * (B, C * K * K, OH * OW) = (B, O, OH * OW)
        let m = op.chan_out;
        let k = op.chan_in * op.kernel * op.kernel;
        let n = op.h_out * op.w_out;
        unsafe {
            sgemm_batch(
                self.blas.as_ref(),
                (op.batch, m, k, n),
                rhs.data.as_ref(),
                [0, k, 1],
                &patches,
                [k * n, n, 1],
                Default::default(),
                Arc::make_mut(&mut out.data),
                [m * n, n, 1],
            )
            .unwrap();
        }

        Ok(())
    }

    fn backward<L: Shape, R: Shape, O: Shape>(
        &self,
        op: super::Conv2DOp,
        lhs: &Self::Storage<L, E>,
        grad_lhs: &mut Self::Storage<L, E>,
        rhs: &Self::Storage<R, E>,
        grad_rhs: &mut Self::Storage<R, E>,
        grad_out: &Self::Storage<O, E>,
    ) -> Result<(), Self::Err> {
        let patches_numel = op.batch * op.chan_out * op.kernel * op.kernel * op.h_in * op.w_in;
        let mut patches = self.dev.alloc_zeros_async::<E>(patches_numel)?;

        {
            // unfold grad_out into patches
            let unfold_fn = self.dev.get_func(Self::MOD, Self::FNS[1]).unwrap();
            let cfg = LaunchConfig::for_num_elems(patches_numel as u32);
            let params = (op, grad_out.data.as_ref(), &mut patches);
            unsafe { unfold_fn.launch_async(cfg, params) }?;
        }

        let filters_numel = op.batch * op.chan_in * op.chan_out * op.kernel * op.kernel;
        let mut f_b1023 = self.dev.alloc_zeros_async::<E>(filters_numel)?;
        let mut grad_f_b1023 = self.dev.alloc_zeros_async::<E>(filters_numel)?;
        let f_strides = self.dev.take_async(rhs.strides.into())?;

        {
            // prepare filters for backward operations by
            // swapping dims 0 and 1 and adding a batch dimension
            let tr_fn = self.dev.get_func(Self::MOD, Self::FNS[2]).unwrap();
            let cfg = LaunchConfig::for_num_elems(rhs.shape.num_elements() as u32);
            let params = (op, rhs.data.as_ref(), &f_strides, &mut f_b1023);
            unsafe { tr_fn.launch_async(cfg, params) }?;
        }

        {
            // img_g += filters * patches
            // (B, C, H * W) += (B, C, O * K * K) * (B, O * K * K, H * W)
            let m = op.chan_in;
            let k = op.chan_out * op.kernel * op.kernel;
            let n = op.h_in * op.w_in;
            unsafe {
                sgemm_batch(
                    self.blas.as_ref(),
                    (op.batch, m, k, n),
                    &f_b1023,
                    [m * k, k, 1],
                    &patches,
                    [k * n, n, 1],
                    <E>::ONE,
                    Arc::make_mut(&mut grad_lhs.data),
                    [m * n, n, 1],
                )
                .unwrap();
            }
        }

        {
            // weight_g += img * patches^T
            // (B, C, O * K * K) += (B, C, H * W) * (B, H * W, O * K * K)
            let m = op.chan_in;
            let k = op.h_in * op.w_in;
            let n = op.chan_out * op.kernel * op.kernel;
            unsafe {
                sgemm_batch(
                    self.blas.as_ref(),
                    (op.batch, m, k, n),
                    lhs.data.as_ref(),
                    [m * k, k, 1],
                    &patches,
                    [k * n, 1, k],
                    <E>::ONE,
                    &mut grad_f_b1023,
                    [m * n, n, 1],
                )
                .unwrap();
            }

            // sum all the gradients collected in our broadcasted grad_f
            // into grad_rhs
            let sum_fn = self.dev.get_func(Self::MOD, Self::FNS[3]).unwrap();
            let cfg = LaunchConfig::for_num_elems(rhs.shape.num_elements() as u32);
            let params = (
                op,
                &grad_f_b1023,
                Arc::make_mut(&mut grad_rhs.data),
                &f_strides,
            );
            unsafe { sum_fn.launch_async(cfg, params) }?;
        }

        Ok(())
    }
}
